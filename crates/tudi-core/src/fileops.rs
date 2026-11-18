//! Copying and moving files for paste (blocking; run on a worker thread).
//!
//! - Names never clash: if the destination exists, `name (2).ext`,
//!   `name (3).ext`, … is used.
//! - Directories are copied recursively; symlinks are copied as links
//!   (never followed); permissions are kept; FIFOs, sockets and devices are
//!   skipped.
//! - A move is a `rename(2)`; across filesystems it falls back to copy and
//!   delete. Moving an item into the folder it is already in does nothing.
//! - Copying or moving a folder into itself (or below it) is refused.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

/// Copy or move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Move,
}

/// `dir/name`, or `dir/name (n).ext` with the first free `n ≥ 2`.
pub fn unique_dest(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let bytes = name.as_bytes();
    // Keep the extension after the last dot, unless the dot is the first
    // character (".bashrc") or there is none.
    let (stem, ext) = match bytes.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => (&bytes[..p], &bytes[p..]),
        _ => (bytes, &b""[..]),
    };
    (2..)
        .map(|n| {
            let mut v = stem.to_vec();
            v.extend_from_slice(format!(" ({n})").as_bytes());
            v.extend_from_slice(ext);
            dir.join(OsString::from_vec(v))
        })
        .find(|p| fs::symlink_metadata(p).is_err())
        .expect("some name is free")
}

fn refuse_into_itself(src: &Path, dest_dir: &Path) -> io::Result<()> {
    let s = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let d = fs::canonicalize(dest_dir).unwrap_or_else(|_| dest_dir.to_path_buf());
    if fs::symlink_metadata(src)?.is_dir() && d.starts_with(&s) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot put {} inside itself", src.display()),
        ));
    }
    Ok(())
}

/// Copy `src` recursively to `dest` (which must not exist).
fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        symlink(fs::read_link(src)?, dest)
    } else if ft.is_dir() {
        fs::create_dir(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
        fs::set_permissions(dest, fs::Permissions::from_mode(meta.permissions().mode()))
    } else if ft.is_file() {
        fs::copy(src, dest).map(|_| ())
    } else {
        Ok(()) // FIFO, socket, device: skipped
    }
}

fn remove_tree(p: &Path) -> io::Result<()> {
    if fs::symlink_metadata(p)?.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// Copy or move `src` into `dest_dir`. Returns the new path.
pub fn transfer(src: &Path, dest_dir: &Path, mode: Mode) -> io::Result<PathBuf> {
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    if mode == Mode::Move && src.parent() == Some(dest_dir) {
        return Ok(src.to_path_buf());
    }
    refuse_into_itself(src, dest_dir)?;
    let dest = unique_dest(dest_dir, name);
    match mode {
        Mode::Copy => {
            if let Err(e) = copy_tree(src, &dest) {
                let _ = remove_tree(&dest);
                return Err(e);
            }
        }
        Mode::Move => match fs::rename(src, &dest) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(18) => {
                // EXDEV: across filesystems.
                if let Err(e) = copy_tree(src, &dest) {
                    let _ = remove_tree(&dest);
                    return Err(e);
                }
                remove_tree(src)?;
            }
            Err(e) => return Err(e),
        },
    }
    Ok(dest)
}

/// Result of transferring several items.
#[derive(Debug, Default)]
pub struct Report {
    pub done: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, io::Error)>,
}

/// Transfer each source into `dest_dir`, continuing after failures.
pub fn transfer_all(srcs: &[PathBuf], dest_dir: &Path, mode: Mode) -> Report {
    let mut r = Report::default();
    for s in srcs {
        match transfer(s, dest_dir, mode) {
            Ok(p) => r.done.push(p),
            Err(e) => r.failed.push((s.clone(), e)),
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn unique_names() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert_eq!(unique_dest(d, "a.txt".as_ref()), d.join("a.txt"));
        write(&d.join("a.txt"), "");
        write(&d.join("a (2).txt"), "");
        assert_eq!(unique_dest(d, "a.txt".as_ref()), d.join("a (3).txt"));
        write(&d.join(".bashrc"), "");
        assert_eq!(unique_dest(d, ".bashrc".as_ref()), d.join(".bashrc (2)"));
        fs::create_dir(d.join("dir")).unwrap();
        assert_eq!(unique_dest(d, "dir".as_ref()), d.join("dir (2)"));
    }

    #[test]
    fn copy_tree_with_symlink_and_mode() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write(&src.join("sub/f.txt"), "hello");
        symlink("sub/f.txt", src.join("link")).unwrap();
        symlink("/nonexistent", src.join("dangling")).unwrap();
        fs::set_permissions(src.join("sub/f.txt"), fs::Permissions::from_mode(0o750)).unwrap();
        let dest_dir = t.path().join("out");
        fs::create_dir(&dest_dir).unwrap();
        let d = transfer(&src, &dest_dir, Mode::Copy).unwrap();
        assert_eq!(d, dest_dir.join("src"));
        assert_eq!(fs::read_to_string(d.join("sub/f.txt")).unwrap(), "hello");
        assert_eq!(fs::read_link(d.join("link")).unwrap(), Path::new("sub/f.txt"));
        assert_eq!(fs::read_link(d.join("dangling")).unwrap(), Path::new("/nonexistent"));
        let mode = fs::metadata(d.join("sub/f.txt")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o750);
        assert!(src.exists(), "copy keeps the source");
        // Pasting again gets a new name.
        assert_eq!(transfer(&src, &dest_dir, Mode::Copy).unwrap(), dest_dir.join("src (2)"));
    }

    #[test]
    fn move_and_same_folder_noop() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a.txt");
        write(&a, "x");
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        assert_eq!(transfer(&a, t.path(), Mode::Move).unwrap(), a, "same folder");
        let moved = transfer(&a, &out, Mode::Move).unwrap();
        assert_eq!(moved, out.join("a.txt"));
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(moved).unwrap(), "x");
    }

    #[test]
    fn refuses_into_itself() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir_all(d.join("inner")).unwrap();
        for mode in [Mode::Copy, Mode::Move] {
            let e = transfer(&d, &d.join("inner"), mode).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        }
        // Copying a folder next to itself is fine.
        assert_eq!(transfer(&d, t.path(), Mode::Copy).unwrap(), t.path().join("d (2)"));
    }

    #[test]
    fn transfer_all_reports_failures() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        write(&a, "");
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = transfer_all(&[a, t.path().join("missing")], &out, Mode::Copy);
        assert_eq!(r.done.len(), 1);
        assert_eq!(r.failed.len(), 1);
    }
}
