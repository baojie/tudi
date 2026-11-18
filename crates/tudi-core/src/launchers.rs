//! Launchers (ADR-014): scripts and applications pinned to the bookmarks
//! bar; clicking one runs it. Stored in `$XDG_CONFIG_HOME/tudi/launchers.toml`:
//!
//! ```toml
//! [[launcher]]
//! path = "/home/user/bin/backup.sh"
//! name = "Backup"            # optional label; default: the file name
//! mode = "terminal"          # terminal (default) | background
//! args = ["--fast"]          # optional fixed arguments
//! cwd = "/home/user"         # optional; default: the script's folder
//! ```
//!
//! [`plan`] turns a launcher into a command line; it does no I/O, so the
//! caller decides executability (a `stat`) off the GUI thread.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    /// In a terminal window that stays open afterwards.
    #[default]
    Terminal,
    /// Without a window; output goes to a log file.
    Background,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launcher {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub mode: RunMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl Launcher {
    pub fn label(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        let p = Path::new(&self.path);
        let name = p
            .file_name()
            .map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned());
        // "backup.sh" → "backup"; "app.desktop" → "app".
        match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem.to_string(),
            _ => name,
        }
    }

    pub fn is_desktop_file(&self) -> bool {
        self.path.ends_with(".desktop")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launchers {
    #[serde(default, rename = "launcher")]
    pub items: Vec<Launcher>,
}

impl Launchers {
    pub fn path() -> Option<PathBuf> {
        crate::config::config_dir().map(|d| d.join("launchers.toml"))
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Add a file. False if already there or not UTF-8.
    pub fn add(&mut self, file: &Path) -> bool {
        let Some(p) = file.to_str() else { return false };
        if self.items.iter().any(|l| l.path == p) {
            return false;
        }
        self.items.push(Launcher {
            path: p.into(),
            name: None,
            mode: RunMode::Terminal,
            args: Vec::new(),
            cwd: None,
        });
        true
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.items.len() {
            self.items.remove(i);
        }
    }

    pub fn move_to(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to < self.items.len() && from != to {
            let l = self.items.remove(from);
            self.items.insert(to, l);
        }
    }
}

/// Can this file be a launcher? (Decided by name and mode bits.)
pub fn is_launchable(name: &str, mode: u32) -> bool {
    let regular = mode & 0o170000 == 0o100000;
    regular
        && (mode & 0o111 != 0
            || name.ends_with(".desktop")
            || name.ends_with(".sh")
            || name.ends_with(".py"))
}

/// A command line to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Wrap in the terminal command (`commands.terminal_exec`).
    pub terminal: bool,
}

/// How to run `l` with extra file arguments. `executable`: the file has an
/// execute bit (the caller `stat`s it). Desktop files go through
/// `gio launch` and never use a terminal.
pub fn plan(l: &Launcher, files: &[PathBuf], executable: bool) -> Plan {
    let path = PathBuf::from(&l.path);
    let cwd = l.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| {
        path.parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf)
    });
    let extra = files.iter().map(|f| f.to_string_lossy().into_owned());
    if l.is_desktop_file() {
        return Plan {
            program: "gio".into(),
            args: ["launch".to_string(), l.path.clone()]
                .into_iter()
                .chain(extra)
                .collect(),
            cwd,
            terminal: false,
        };
    }
    let (program, mut args) = if executable {
        (l.path.clone(), Vec::new())
    } else if l.path.ends_with(".py") {
        ("python3".to_string(), vec![l.path.clone()])
    } else {
        ("sh".to_string(), vec![l.path.clone()])
    };
    args.extend(l.args.iter().cloned());
    args.extend(extra);
    Plan {
        program,
        args,
        cwd,
        terminal: l.mode == RunMode::Terminal,
    }
}

/// Wrap a plan in the terminal template: `{dir}` → the working folder,
/// an argument that is exactly `{cmd}` → program and arguments.
pub fn in_terminal(template: &[String], p: &Plan) -> Option<(String, Vec<String>)> {
    let dir = p.cwd.to_string_lossy();
    let mut out: Vec<String> = Vec::new();
    for a in template {
        if a == "{cmd}" {
            out.push(p.program.clone());
            out.extend(p.args.iter().cloned());
        } else {
            out.push(a.replace("{dir}", &dir));
        }
    }
    if out.is_empty() {
        return None;
    }
    let program = out.remove(0);
    Some((program, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(path: &str) -> Launcher {
        Launcher {
            path: path.into(),
            name: None,
            mode: RunMode::Terminal,
            args: vec![],
            cwd: None,
        }
    }

    #[test]
    fn labels_and_store() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("launchers.toml");
        let mut ls = Launchers::load(&f);
        assert!(ls.add(Path::new("/w/bin/backup.sh")));
        assert!(ls.add(Path::new("/w/apps/editor.desktop")));
        assert!(!ls.add(Path::new("/w/bin/backup.sh")));
        ls.items[0].mode = RunMode::Background;
        ls.items[0].args = vec!["-v".into()];
        assert_eq!(ls.items[0].label(), "backup");
        assert_eq!(ls.items[1].label(), "editor");
        ls.move_to(1, 0);
        ls.save(&f).unwrap();
        assert_eq!(Launchers::load(&f), ls);
        ls.remove(0);
        assert_eq!(ls.items.len(), 1);
    }

    #[test]
    fn plans() {
        let files = [PathBuf::from("/d/a b.txt")];
        let p = plan(&l("/w/bin/run"), &files, true);
        assert_eq!(p.program, "/w/bin/run");
        assert_eq!(p.args, ["/d/a b.txt"]);
        assert_eq!(p.cwd, Path::new("/w/bin"));
        assert!(p.terminal);

        let mut py = l("/w/x.py");
        py.args = vec!["--n".into()];
        py.mode = RunMode::Background;
        py.cwd = Some("/tmp".into());
        let p = plan(&py, &[], false);
        assert_eq!(
            (p.program.as_str(), p.args.clone()),
            ("python3", vec!["/w/x.py".into(), "--n".into()])
        );
        assert_eq!(p.cwd, Path::new("/tmp"));
        assert!(!p.terminal);

        assert_eq!(plan(&l("/w/x.sh"), &[], false).program, "sh");

        let p = plan(&l("/a/app.desktop"), &files, false);
        assert_eq!(p.program, "gio");
        assert_eq!(p.args, ["launch", "/a/app.desktop", "/d/a b.txt"]);
        assert!(!p.terminal);
    }

    #[test]
    fn terminal_wrap() {
        let p = plan(&l("/w/bin/run"), &[PathBuf::from("/f")], true);
        let t: Vec<String> = ["kitty", "--directory", "{dir}", "--hold", "{cmd}"]
            .map(String::from)
            .to_vec();
        let (prog, args) = in_terminal(&t, &p).unwrap();
        assert_eq!(prog, "kitty");
        assert_eq!(
            args,
            ["--directory", "/w/bin", "--hold", "/w/bin/run", "/f"]
        );
    }

    #[test]
    fn launchable() {
        assert!(is_launchable("x", 0o100755));
        assert!(is_launchable("a.sh", 0o100644));
        assert!(is_launchable("a.desktop", 0o100644));
        assert!(!is_launchable("a.txt", 0o100644));
        assert!(!is_launchable("dir", 0o040755));
    }
}
