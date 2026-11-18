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
editor = ["zed", "{path}"]

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
    pub editor: Vec<String>,
}

impl Default for Commands {
    fn default() -> Self {
        Self {
            terminal: vec!["kitty".into(), "--directory".into(), "{dir}".into()],
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
