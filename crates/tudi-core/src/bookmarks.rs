//! Bookmarks: folders pinned to the bookmarks bar, in the user's order.
//! Stored in `$XDG_CONFIG_HOME/tudi/bookmarks.toml`.
//!
//! ```toml
//! [[bookmark]]
//! path = "/home/user/projects/acme"
//! name = "acme"          # optional label; default: the folder name
//! ```
//!
//! Paths that are not valid UTF-8 cannot be stored in TOML; such folders
//! cannot be bookmarked (callers check [`Bookmarks::add`]'s result).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Bookmark {
    /// The label shown on the bar.
    pub fn label(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        Path::new(&self.path)
            .file_name()
            .map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmarks {
    /// Shown in the saved order (the user dragged them) rather than by
    /// open count (ADR-023).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual_order: bool,
    #[serde(default, rename = "bookmark")]
    pub items: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn path() -> Option<PathBuf> {
        crate::config::config_dir().map(|d| d.join("bookmarks.toml"))
    }

    /// Load; a missing or invalid file gives no bookmarks.
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

    pub fn contains(&self, dir: &Path) -> bool {
        dir.to_str()
            .is_some_and(|p| self.items.iter().any(|b| b.path == p))
    }

    /// Append `dir`. False if already bookmarked or not UTF-8.
    pub fn add(&mut self, dir: &Path) -> bool {
        let Some(p) = dir.to_str() else { return false };
        if self.contains(dir) {
            return false;
        }
        self.items.push(Bookmark {
            path: p.into(),
            name: None,
        });
        true
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }

    /// Set a custom label; an empty label restores the folder name.
    pub fn rename(&mut self, index: usize, name: &str) {
        if let Some(b) = self.items.get_mut(index) {
            let name = name.trim();
            b.name = (!name.is_empty()).then(|| name.to_string());
        }
    }

    /// Fix the shown `order` (indices into `items`) as the saved order and
    /// move the bookmark shown at `from` to shown position `to`: the
    /// bookmarks are in manual order from now on.
    pub fn reorder(&mut self, order: &[usize], from: usize, to: usize) {
        let mut items: Vec<Bookmark> = order
            .iter()
            .filter_map(|&i| self.items.get(i).cloned())
            .collect();
        if items.len() != self.items.len() {
            return;
        }
        if from < items.len() && to < items.len() {
            let b = items.remove(from);
            items.insert(to, b);
        }
        self.items = items;
        self.manual_order = true;
    }

    /// Move the bookmark at `from` so it ends up at index `to`.
    pub fn move_to(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to < self.items.len() && from != to {
            let b = self.items.remove(from);
            self.items.insert(to, b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_fixes_the_shown_order() {
        let mut b = Bookmarks::default();
        for p in ["/a", "/b", "/c"] {
            b.add(Path::new(p));
        }
        // Shown by count as c, a, b; drag b (shown 2) to the front.
        b.reorder(&[2, 0, 1], 2, 0);
        let paths: Vec<&str> = b.items.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, ["/b", "/c", "/a"]);
        assert!(b.manual_order);
        let text = toml::to_string(&b).unwrap();
        assert!(text.contains("manual_order = true"), "{text}");
        assert_eq!(toml::from_str::<Bookmarks>(&text).unwrap(), b);
        b.manual_order = false;
        assert!(!toml::to_string(&b).unwrap().contains("manual_order"));
    }
    use std::os::unix::ffi::OsStrExt;

    fn labels(b: &Bookmarks) -> Vec<String> {
        b.items.iter().map(Bookmark::label).collect()
    }

    #[test]
    fn add_rename_move_remove_and_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("c/bookmarks.toml");
        let mut b = Bookmarks::load(&file);
        assert!(b.add(Path::new("/w/acme")));
        assert!(b.add(Path::new("/w/合同")));
        assert!(b.add(Path::new("/")));
        assert!(!b.add(Path::new("/w/acme")), "no duplicates");
        assert!(!b.add(&PathBuf::from(std::ffi::OsStr::from_bytes(b"/w/caf\xe9"))));
        assert_eq!(labels(&b), ["acme", "合同", "/"]);
        b.rename(0, "  Work  ");
        b.move_to(2, 0);
        assert_eq!(labels(&b), ["/", "Work", "合同"]);
        b.rename(1, "");
        b.remove(0);
        b.remove(9);
        assert_eq!(labels(&b), ["acme", "合同"]);
        b.save(&file).unwrap();
        assert_eq!(Bookmarks::load(&file), b);
        std::fs::write(&file, "not toml [").unwrap();
        assert_eq!(Bookmarks::load(&file), Bookmarks::default());
    }
}
