//! Window state restored across sessions: size, last folder, expanded tree
//! folders. Stored in `$XDG_STATE_HOME/tudi/state.toml`.
//!
//! Paths that are not valid UTF-8 cannot be stored in TOML and are skipped.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Most expanded folders remembered.
pub const MAX_EXPANDED: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    pub last_dir: Option<String>,
    pub expanded: Vec<String>,
    /// Interface zoom in percent.
    pub zoom: u32,
    /// Width of the folder tree pane in pixels.
    pub tree_width: i32,
    /// Widths of the list's size and date columns in pixels (0: default).
    pub size_column: i32,
    pub date_column: i32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1100,
            height: 700,
            maximized: false,
            last_dir: None,
            expanded: Vec::new(),
            zoom: 100,
            tree_width: 280,
            size_column: 0,
            date_column: 0,
        }
    }
}

impl WindowState {
    pub fn path() -> Option<PathBuf> {
        crate::config::state_dir().map(|d| d.join("state.toml"))
    }

    /// Load from `path`; missing or invalid files give the defaults.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Write atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Set the remembered paths, skipping non-UTF-8 ones.
    pub fn set_paths(&mut self, last_dir: &Path, expanded: &[PathBuf]) {
        self.last_dir = last_dir.to_str().map(String::from);
        self.expanded = expanded
            .iter()
            .filter_map(|p| p.to_str().map(String::from))
            .take(MAX_EXPANDED)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn round_trip_and_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("s/state.toml");
        assert_eq!(WindowState::load(&p), WindowState::default());
        let mut s = WindowState {
            width: 800,
            maximized: true,
            ..Default::default()
        };
        let bad = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9"));
        s.set_paths(Path::new("/tmp/x"), &[PathBuf::from("/tmp/合同"), bad]);
        s.save(&p).unwrap();
        let back = WindowState::load(&p);
        assert_eq!(back, s);
        assert_eq!(back.expanded, ["/tmp/合同"]);
        std::fs::write(&p, "width = \"oops\"").unwrap();
        assert_eq!(WindowState::load(&p), WindowState::default());
    }
}
