//! First-run imports (PLAN §5.5, M4): folder weights from autojump and
//! zoxide into the history, and GTK bookmarks into the bookmarks bar.
//! Every source is optional and silently skipped if absent.
//!
//! - autojump: `~/.local/share/autojump/autojump.txt`, lines `weight\tpath`.
//! - zoxide: the output of `zoxide query -ls`, lines `score path`.
//! - GTK: `~/.config/gtk-3.0/bookmarks`, lines `file:///path [label]`.

use std::path::{Path, PathBuf};

/// A folder with a weight from another tool.
#[derive(Clone, Debug, PartialEq)]
pub struct Weighted {
    pub path: PathBuf,
    pub weight: f64,
}

/// Parse autojump's `autojump.txt`.
pub fn parse_autojump(text: &str) -> Vec<Weighted> {
    text.lines()
        .filter_map(|l| {
            let (w, p) = l.split_once('\t')?;
            Some(Weighted {
                weight: w.trim().parse().ok()?,
                path: PathBuf::from(p.trim_end_matches(['\r', '\n'])),
            })
        })
        .filter(|w| w.path.is_absolute() && w.weight > 0.0)
        .collect()
}

/// Parse `zoxide query -ls` output.
pub fn parse_zoxide(text: &str) -> Vec<Weighted> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (w, p) = l.split_once(' ')?;
            Some(Weighted {
                weight: w.trim().parse().ok()?,
                path: PathBuf::from(p.trim_start()),
            })
        })
        .filter(|w| w.path.is_absolute() && w.weight > 0.0)
        .collect()
}

/// Decode `%XX` escapes of a `file://` URI path (bytes, not UTF-8).
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Parse GTK's `bookmarks` file: local folders and their labels.
pub fn parse_gtk_bookmarks(text: &str) -> Vec<(PathBuf, Option<String>)> {
    use std::os::unix::ffi::OsStringExt;
    text.lines()
        .filter_map(|l| {
            let (uri, label) = match l.split_once(' ') {
                Some((u, n)) => (u, Some(n.trim().to_string()).filter(|n| !n.is_empty())),
                None => (l.trim(), None),
            };
            let rest = uri.strip_prefix("file://")?;
            let path = PathBuf::from(std::ffi::OsString::from_vec(percent_decode(rest)));
            path.is_absolute().then_some((path, label))
        })
        .collect()
}

/// Map foreign weights to a number of visits (1..=50) for the history:
/// the heaviest folder gets 50, others proportionally, at least one.
pub fn to_visits(items: &[Weighted]) -> Vec<(PathBuf, u32)> {
    let max = items.iter().map(|w| w.weight).fold(0.0, f64::max);
    if max <= 0.0 {
        return Vec::new();
    }
    items
        .iter()
        .map(|w| {
            let v = (w.weight / max * 50.0).round().clamp(1.0, 50.0) as u32;
            (w.path.clone(), v)
        })
        .collect()
}

/// Default locations of the sources.
pub fn autojump_file(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("autojump/autojump.txt")
}

pub fn gtk_bookmarks_file(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("gtk-3.0/bookmarks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn autojump_fixture() {
        let t = "30.0\t/home/u/projects/acme\n10.5\t/tmp\nbad line\n-1\t/x\n5\trelative/path\n";
        let v = parse_autojump(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].path, Path::new("/home/u/projects/acme"));
        assert_eq!(v[1].weight, 10.5);
    }

    #[test]
    fn zoxide_fixture() {
        let t = "  42.0 /home/u/projects/acme\n   0.5 /home/u/with space\nnot a line\n";
        let v = parse_zoxide(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].path, Path::new("/home/u/with space"));
    }

    #[test]
    fn gtk_fixture() {
        let t = "file:///home/u/Documents\nfile:///home/u/%E5%90%88%E5%90%8C Contracts\nsftp://host/x\nfile:///home/u/caf%E9\n";
        let v = parse_gtk_bookmarks(t);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], (PathBuf::from("/home/u/Documents"), None));
        assert_eq!(
            v[1],
            (PathBuf::from("/home/u/合同"), Some("Contracts".into()))
        );
        assert_eq!(v[2].0.as_os_str().as_bytes(), b"/home/u/caf\xe9");
    }

    #[test]
    fn weights_to_visits() {
        let v = to_visits(&[
            Weighted {
                path: "/a".into(),
                weight: 100.0,
            },
            Weighted {
                path: "/b".into(),
                weight: 1.0,
            },
        ]);
        assert_eq!(v, [(PathBuf::from("/a"), 50), (PathBuf::from("/b"), 1)]);
        assert!(to_visits(&[]).is_empty());
    }
}
