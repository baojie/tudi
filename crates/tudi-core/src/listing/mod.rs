//! Directory listing (PLAN §5.1).
//!
//! [`Lister::list`] reads one directory with `getdents64` into a [`Listing`]:
//! names in one arena plus the `d_type` of each entry, with no `stat`.
//! Sizes, times and unresolved types come later from [`meta`], sorting from
//! [`sort`]. Everything here runs on worker threads, never on the GTK main
//! thread.

pub mod meta;
pub mod natural;
pub mod sort;

use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::{FileType, Mode, OFlags, RawDir};

/// Entry type as reported by `getdents64` (or resolved later by `statx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    /// FIFO, socket, or device.
    Other,
    /// The filesystem returned `DT_UNKNOWN`; resolved in the metadata phase.
    Unknown,
}

impl Kind {
    fn from_file_type(t: FileType) -> Self {
        match t {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Dir,
            FileType::Symlink => Kind::Symlink,
            FileType::Unknown => Kind::Unknown,
            _ => Kind::Other,
        }
    }
}

/// Error reading a directory.
#[derive(Debug, thiserror::Error)]
#[error("cannot list {path}: {source}")]
pub struct ListError {
    pub path: PathBuf,
    #[source]
    pub source: io::Error,
}

impl ListError {
    fn new(path: &Path, errno: rustix::io::Errno) -> Self {
        Self {
            path: path.to_owned(),
            source: errno.into(),
        }
    }

    /// The error kind, e.g. `PermissionDenied` for a locked directory.
    pub fn kind(&self) -> io::ErrorKind {
        self.source.kind()
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// Offset of the name in the arena; the name is followed by a NUL.
    off: u32,
    len: u32,
    kind: Kind,
}

/// The entries of one directory, in `getdents64` order.
///
/// Names are raw bytes (`OsStr`), never forced to UTF-8. The directory stays
/// open so metadata can be fetched relative to it (`statx` with the dirfd),
/// which needs no path joining and is immune to renames of the directory.
#[derive(Debug)]
pub struct Listing {
    /// Matches requests to results; stale generations are dropped.
    pub generation: u64,
    dir: OwnedFd,
    names: Vec<u8>,
    entries: Vec<Entry>,
}

impl Listing {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Name bytes of entry `i`.
    pub fn name(&self, i: usize) -> &[u8] {
        let e = &self.entries[i];
        &self.names[e.off as usize..(e.off + e.len) as usize]
    }

    /// Name of entry `i` as a C string, for `*at` syscalls.
    pub fn name_cstr(&self, i: usize) -> &CStr {
        let e = &self.entries[i];
        let bytes = &self.names[e.off as usize..=(e.off + e.len) as usize];
        CStr::from_bytes_with_nul(bytes).expect("arena names are NUL-terminated")
    }

    /// Type of entry `i` from `getdents64`; may be [`Kind::Unknown`].
    pub fn kind(&self, i: usize) -> Kind {
        self.entries[i].kind
    }

    /// The open directory.
    pub fn dir(&self) -> &OwnedFd {
        &self.dir
    }

    /// Build a listing from given entries over an open directory. Used to
    /// simulate filesystems that return `DT_UNKNOWN`.
    #[cfg(test)]
    pub(crate) fn from_parts(dir: OwnedFd, entries: &[(&[u8], Kind)]) -> Self {
        let mut l = Self {
            generation: 0,
            dir,
            names: Vec::new(),
            entries: Vec::new(),
        };
        for (name, kind) in entries {
            push_entry(&mut l.names, &mut l.entries, name, *kind).unwrap();
        }
        l
    }
}

fn push_entry(
    names: &mut Vec<u8>,
    entries: &mut Vec<Entry>,
    name: &[u8],
    kind: Kind,
) -> Result<(), rustix::io::Errno> {
    // Name arenas over 4 GiB cannot happen with NAME_MAX = 255 and any real
    // directory, but do not wrap silently.
    let off = u32::try_from(names.len()).map_err(|_| rustix::io::Errno::OVERFLOW)?;
    names.extend_from_slice(name);
    names.push(0);
    entries.push(Entry {
        off,
        len: name.len() as u32,
        kind,
    });
    Ok(())
}

/// Size of the reusable `getdents64` buffer.
const BUF_SIZE: usize = 256 * 1024;

/// Reads directories with a reusable buffer. One per worker thread.
pub struct Lister {
    buf: Vec<MaybeUninit<u8>>,
}

impl Default for Lister {
    fn default() -> Self {
        Self::new()
    }
}

impl Lister {
    pub fn new() -> Self {
        let mut buf = Vec::with_capacity(BUF_SIZE);
        buf.resize(BUF_SIZE, MaybeUninit::uninit());
        Self { buf }
    }

    /// List `path` (following a symlink to it, since the user asked for it).
    /// Entries are not followed or `stat`ed. `.` and `..` are skipped.
    pub fn list(&mut self, path: &Path, generation: u64) -> Result<Listing, ListError> {
        let dir = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| ListError::new(path, e))?;
        let mut listing = Listing {
            generation,
            dir,
            names: Vec::new(),
            entries: Vec::new(),
        };
        // `raw` borrows only the `dir` field, so the arena can grow meanwhile.
        let mut raw = RawDir::new(&listing.dir, &mut self.buf);
        while let Some(entry) = raw.next() {
            let entry = entry.map_err(|e| ListError::new(path, e))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            push_entry(
                &mut listing.names,
                &mut listing.entries,
                name,
                Kind::from_file_type(entry.file_type()),
            )
            .map_err(|e| ListError::new(path, e))?;
        }
        Ok(listing)
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn touch(dir: &Path, name: &[u8], size: usize) {
        fs::write(
            dir.join(std::ffi::OsStr::from_bytes(name)),
            vec![b'x'; size],
        )
        .unwrap();
    }

    /// True when running as root (permission tests are meaningless then).
    pub fn is_root() -> bool {
        use std::os::unix::fs::MetadataExt;
        // /proc/self is owned by the effective uid.
        std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn entries(l: &Listing) -> BTreeMap<Vec<u8>, Kind> {
        (0..l.len())
            .map(|i| (l.name(i).to_vec(), l.kind(i)))
            .collect()
    }

    #[test]
    fn lists_names_and_types_without_dot_entries() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), b"a.txt", 3);
        fs::create_dir(t.path().join("sub")).unwrap();
        symlink("a.txt", t.path().join("link")).unwrap();
        let l = Lister::new().list(t.path(), 7).unwrap();
        assert_eq!(l.generation, 7);
        let e = entries(&l);
        assert_eq!(e.len(), 3);
        assert_eq!(e[&b"a.txt"[..]], Kind::File);
        assert_eq!(e[&b"sub"[..]], Kind::Dir);
        assert_eq!(e[&b"link"[..]], Kind::Symlink);
        for i in 0..l.len() {
            assert_eq!(l.name_cstr(i).to_bytes(), l.name(i));
        }
    }

    #[test]
    fn empty_directory() {
        let t = tempfile::tempdir().unwrap();
        let l = Lister::new().list(t.path(), 0).unwrap();
        assert!(l.is_empty());
    }

    #[test]
    fn non_utf8_and_cjk_names_are_kept_as_bytes() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), b"caf\xe9.txt", 0);
        touch(t.path(), "合同核对.xlsx".as_bytes(), 0);
        let e = entries(&Lister::new().list(t.path(), 0).unwrap());
        assert!(e.contains_key(&b"caf\xe9.txt"[..]));
        assert!(e.contains_key("合同核对.xlsx".as_bytes()));
    }

    #[test]
    fn symlink_loops_are_listed_not_followed() {
        let t = tempfile::tempdir().unwrap();
        symlink("b", t.path().join("a")).unwrap();
        symlink("a", t.path().join("b")).unwrap();
        symlink("..", t.path().join("up")).unwrap();
        let e = entries(&Lister::new().list(t.path(), 0).unwrap());
        assert_eq!(e.len(), 3);
        assert!(e.values().all(|&k| k == Kind::Symlink));
    }

    #[test]
    fn more_entries_than_one_buffer() {
        let t = tempfile::tempdir().unwrap();
        // ~5000 entries with long names exceed one 256 KiB getdents buffer.
        for i in 0..5000 {
            touch(t.path(), format!("{i:0>60}").as_bytes(), 0);
        }
        let l = Lister::new().list(t.path(), 0).unwrap();
        assert_eq!(l.len(), 5000);
    }

    #[test]
    fn permission_denied_is_an_error() {
        if is_root() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let locked = t.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let err = Lister::new().list(&locked, 0).unwrap_err();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(err.path, locked);
    }

    #[test]
    fn missing_directory_and_file_are_errors() {
        let t = tempfile::tempdir().unwrap();
        let err = Lister::new().list(&t.path().join("nope"), 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        touch(t.path(), b"f", 0);
        let err = Lister::new().list(&t.path().join("f"), 0).unwrap_err();
        assert_eq!(err.source.raw_os_error(), Some(20)); // ENOTDIR
    }
}
