//! `config.toml` (PLAN §8): all keys optional; defaults written on first run.
//!
//! Only the sections used so far are typed here; unknown keys and sections
//! (e.g. `[index]` before M5) are ignored, so a config written for a newer
//! version still loads.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file written on first run. Must parse to `Config::default()`
/// (checked by a test).
pub const DEFAULT_TOML: &str = r#"# tudi configuration. All keys are optional; these are the defaults.

[ui]
# "en": English. "system": follow the system language (LANG / LANGUAGE),
# using the translations shipped with tudi (e.g. zh_CN).
language = "en"

[browse]
show_hidden = false
dirs_first = true
sort = "name"          # name | size | mtime

[commands]
# {dir}: the current folder. {path}: each selected item (or the current
# folder if nothing is selected), as separate arguments.
terminal = ["kitty", "--directory", "{dir}"]
# Run a launcher in a terminal that stays open: {cmd} = program and args.
terminal_exec = ["kitty", "--directory", "{dir}", "--hold", "{cmd}"]
editor = ["zed", "{path}"]

[index]
# Folders the search index covers ("~" is your home folder).
roots = ["~"]
# Folders indexed themselves but not their contents. A pattern starting
# with "/" is an absolute glob; otherwise it matches anywhere.
exclude = [".cache", ".local/share/Trash", "node_modules", "target", "__pycache__", ".venv", ".git/objects", "snap", ".npm", ".cargo/registry"]
# Minutes between checks for changes in folders that are not watched.
reconcile_minutes = 10

# More roots with their own settings, e.g. a data disk or an SD card.
# Slow (rotational or USB) devices are checked once a day by default.
# While a root is unmounted its entries are kept and hidden from results.
# [[index.extra_roots]]
# path = "/mnt/data"
# reconcile_minutes = 1440
# exclude = ["$RECYCLE.BIN", "System Volume Information"]

[history]
# Folders never recorded or shown in the history, as globs on absolute
# paths, e.g. ["/tmp/**", "**/node_modules/**"]. Single folders can also be
# excluded from the context menu.
exclude = []
"#;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    #[default]
    #[serde(alias = "natural")]
    Name,
    Size,
    Mtime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Browse {
    pub show_hidden: bool,
    pub dirs_first: bool,
    pub sort: SortBy,
}

impl Default for Browse {
    fn default() -> Self {
        Self {
            show_hidden: false,
            dirs_first: true,
            sort: SortBy::Name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Commands {
    pub terminal: Vec<String>,
    pub terminal_exec: Vec<String>,
    pub editor: Vec<String>,
}

impl Default for Commands {
    fn default() -> Self {
        Self {
            terminal: vec!["kitty".into(), "--directory".into(), "{dir}".into()],
            terminal_exec: ["kitty", "--directory", "{dir}", "--hold", "{cmd}"]
                .map(String::from)
                .to_vec(),
            editor: vec!["zed".into(), "{path}".into()],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// English regardless of the system locale.
    #[default]
    En,
    /// Follow the system locale.
    System,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub language: Language,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    pub roots: Vec<String>,
    pub exclude: Vec<String>,
    pub reconcile_minutes: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_roots: Vec<ExtraRoot>,
}

/// An extra index root with its own settings (PLAN §8).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtraRoot {
    pub path: String,
    /// Default: daily on slow (rotational / USB) devices, else the global
    /// interval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconcile_minutes: Option<u32>,
    /// In addition to `[index] exclude`.
    pub exclude: Vec<String>,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            roots: vec!["~".into()],
            exclude: [
                ".cache",
                ".local/share/Trash",
                "node_modules",
                "target",
                "__pycache__",
                ".venv",
                ".git/objects",
                "snap",
                ".npm",
                ".cargo/registry",
            ]
            .map(String::from)
            .to_vec(),
            reconcile_minutes: 10,
            extra_roots: Vec::new(),
        }
    }
}

impl IndexConfig {
    /// Root folders with `~` expanded.
    pub fn root_paths(&self) -> Vec<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        self.roots
            .iter()
            .map(|r| {
                if r == "~" {
                    home.clone()
                } else if let Some(rest) = r.strip_prefix("~/") {
                    home.join(rest)
                } else {
                    PathBuf::from(r)
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: Ui,
    pub browse: Browse,
    pub commands: Commands,
    pub index: IndexConfig,
    pub history: History,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// `$XDG_CONFIG_HOME/tudi`, falling back to `~/.config/tudi`.
pub fn config_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_STATE_HOME/tudi`, falling back to `~/.local/state/tudi`.
pub fn state_dir() -> Option<PathBuf> {
    xdg_dir("XDG_STATE_HOME", ".local/state")
}

fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))?;
    Some(base.join("tudi"))
}

impl Config {
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Load `path`; if it does not exist, write the defaults there (best
    /// effort) and return them.
    pub fn load_or_init(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, DEFAULT_TOML);
                Ok(Self::default())
            }
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Load the user's config (see [`config_dir`]). Errors fall back to the
    /// defaults and are returned for display.
    pub fn load_user() -> (Self, Option<ConfigError>) {
        let Some(dir) = config_dir() else {
            return (Self::default(), None);
        };
        match Self::load_or_init(&dir.join("config.toml")) {
            Ok(c) => (c, None),
            Err(e) => (Self::default(), Some(e)),
        }
    }
}

impl Config {
    /// Write `self` to `path`, keeping the comments and layout of the
    /// existing file (or of [`DEFAULT_TOML`] if there is none): changed
    /// values are replaced in place, new keys are appended to their table.
    pub fn save_preserving(&self, path: &Path) -> std::io::Result<()> {
        let existing = std::fs::read_to_string(path).unwrap_or_else(|_| DEFAULT_TOML.into());
        let mut doc: toml_edit::DocumentMut = existing
            .parse()
            .unwrap_or_else(|_| DEFAULT_TOML.parse().expect("default config parses"));
        let new: toml_edit::DocumentMut = toml::to_string(self)
            .map_err(std::io::Error::other)?
            .parse()
            .map_err(std::io::Error::other)?;
        merge(doc.as_table_mut(), new.as_table());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, doc.to_string())?;
        std::fs::rename(&tmp, path)
    }
}

/// Copy the values of `src` into `dst`, keeping `dst`'s decoration.
fn merge(dst: &mut toml_edit::Table, src: &toml_edit::Table) {
    for (key, item) in src.iter() {
        match (dst.get_mut(key), item) {
            (Some(toml_edit::Item::Table(d)), toml_edit::Item::Table(s)) => merge(d, s),
            (Some(toml_edit::Item::Value(d)), toml_edit::Item::Value(s)) => {
                let decor = d.decor().clone();
                *d = s.clone();
                *d.decor_mut() = decor;
            }
            _ => {
                dst.insert(key, item.clone());
            }
        }
    }
}

/// Expand a command template. `{dir}` is replaced inside any argument; an
/// argument that is exactly `{path}` becomes one argument per path; `{path}`
/// inside a longer argument uses the first path.
pub fn expand_command(
    template: &[String],
    dir: &Path,
    paths: &[PathBuf],
) -> Option<(std::ffi::OsString, Vec<std::ffi::OsString>)> {
    use std::ffi::OsString;
    let first = paths.first().map_or(dir, |p| p.as_path());
    let mut args: Vec<OsString> = Vec::new();
    for arg in template {
        if arg == "{path}" {
            if paths.is_empty() {
                args.push(dir.into());
            } else {
                args.extend(paths.iter().map(OsString::from));
            }
        } else if arg == "{dir}" {
            args.push(dir.into());
        } else {
            // Mixed text: lossy is fine for the surrounding template text.
            let s = arg
                .replace("{dir}", &dir.to_string_lossy())
                .replace("{path}", &first.to_string_lossy());
            args.push(s.into());
        }
    }
    if args.is_empty() {
        return None;
    }
    let program = args.remove(0);
    Some((program, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_defaults() {
        let c = Config::parse(DEFAULT_TOML, Path::new("x")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn partial_and_unknown_keys() {
        let c = Config::parse(
            "[browse]\nshow_hidden = true\nsort = \"natural\"\n[index]\nroots = [\"~\"]\n",
            Path::new("x"),
        )
        .unwrap();
        assert!(c.browse.show_hidden);
        assert!(c.browse.dirs_first);
        assert_eq!(c.browse.sort, SortBy::Name);
        assert_eq!(c.commands, Commands::default());
        let c = Config::parse("[history]\nexclude = [\"/tmp/**\"]\n", Path::new("x")).unwrap();
        assert_eq!(c.history.exclude, ["/tmp/**"]);
        assert_eq!(c.ui.language, Language::En);
        let c = Config::parse(
            "[[index.extra_roots]]\npath = \"/mnt/d\"\nexclude = [\"$RECYCLE.BIN\"]\n",
            Path::new("x"),
        )
        .unwrap();
        assert_eq!(c.index.extra_roots[0].path, "/mnt/d");
        assert_eq!(c.index.extra_roots[0].reconcile_minutes, None);
        assert_eq!(c.index.roots, ["~"], "defaults kept");
        let c = Config::parse("[ui]\nlanguage = \"system\"\n", Path::new("x")).unwrap();
        assert_eq!(c.ui.language, Language::System);
    }

    #[test]
    fn bad_value_is_an_error() {
        let e = Config::parse("[browse]\nsort = \"random\"\n", Path::new("/c.toml")).unwrap_err();
        assert!(e.to_string().contains("/c.toml"));
    }

    #[test]
    fn load_or_init_writes_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sub/config.toml");
        assert_eq!(Config::load_or_init(&p).unwrap(), Config::default());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), DEFAULT_TOML);
        std::fs::write(&p, "[browse]\nshow_hidden = true\n").unwrap();
        assert!(Config::load_or_init(&p).unwrap().browse.show_hidden);
    }

    #[test]
    fn save_keeps_comments() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.toml");
        std::fs::write(&p, "# mine\n[browse]\nshow_hidden = false # keep me\n").unwrap();
        let mut c = Config::load_or_init(&p).unwrap();
        c.browse.show_hidden = true;
        c.ui.language = Language::System;
        c.history.exclude = vec!["/tmp/**".into()];
        c.save_preserving(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains("show_hidden = true # keep me"), "{text}");
        assert_eq!(Config::load_or_init(&p).unwrap(), c);
        // A missing file starts from the commented defaults.
        let q = t.path().join("new/config.toml");
        Config::default().save_preserving(&q).unwrap();
        assert_eq!(std::fs::read_to_string(&q).unwrap(), DEFAULT_TOML);
    }

    #[test]
    fn expand_commands() {
        let dir = Path::new("/w/acme");
        let paths = [PathBuf::from("/w/acme/a b.txt"), PathBuf::from("/w/acme/c")];
        let t: Vec<String> = ["zed", "{path}"].map(String::from).to_vec();
        let (p, a) = expand_command(&t, dir, &paths).unwrap();
        assert_eq!(p, "zed");
        assert_eq!(a, ["/w/acme/a b.txt", "/w/acme/c"]);
        let (_, a) = expand_command(&t, dir, &[]).unwrap();
        assert_eq!(a, ["/w/acme"]);
        let t: Vec<String> = ["kitty", "--directory={dir}"].map(String::from).to_vec();
        let (_, a) = expand_command(&t, dir, &[]).unwrap();
        assert_eq!(a, ["--directory=/w/acme"]);
        assert!(expand_command(&[], dir, &[]).is_none());
    }
}
