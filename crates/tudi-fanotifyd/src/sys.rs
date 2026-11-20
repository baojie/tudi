//! Thin syscall wrappers. Every `unsafe` block of the helper lives here or
//! in `privs.rs`.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::access::{Cred, Meta, parse_acl};

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// `fanotify_init(2)`.
pub fn fanotify_init(flags: u32, event_f_flags: u32) -> io::Result<OwnedFd> {
    // SAFETY: integer arguments only; the returned fd is new and owned.
    let fd = check(unsafe { libc::fanotify_init(flags, event_f_flags) })?;
    // SAFETY: `fd` is a fresh descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `fanotify_mark(2)` on the object at `path`.
pub fn fanotify_mark(fan: &OwnedFd, flags: u32, mask: u64, path: &Path) -> io::Result<()> {
    let c = cstr(path)?;
    // SAFETY: `c` is a valid NUL-terminated string that outlives the call;
    // `fan` is an open fanotify descriptor.
    check(unsafe {
        libc::fanotify_mark(fan.as_raw_fd(), flags, mask, libc::AT_FDCWD, c.as_ptr())
    })?;
    Ok(())
}

/// Open a folder for use as a mount reference (`open_by_handle_at`).
pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let c = cstr(path)?;
    // SAFETY: valid C string; flags are constants; the fd is new.
    let fd = check(unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fsid_of(s: &libc::statfs) -> u64 {
    // SAFETY: `fsid_t` is two `c_int`s (8 bytes) with private fields;
    // reinterpreting it as bytes reads exactly those 8 bytes.
    let raw: [u8; 8] = unsafe { std::mem::transmute(s.f_fsid) };
    u64::from_ne_bytes(raw)
}

/// The filesystem id of the filesystem holding `fd`.
pub fn fsid_fd(fd: &OwnedFd) -> io::Result<u64> {
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `s` is a writable buffer of the right type; `fd` is open.
    check(unsafe { libc::fstatfs(fd.as_raw_fd(), s.as_mut_ptr()) })?;
    // SAFETY: fstatfs succeeded and filled `s`.
    let s = unsafe { s.assume_init() };
    Ok(fsid_of(&s))
}

/// The filesystem id of the filesystem holding `path` (client side: the
/// same value the helper reports in `Covered`).
pub fn fsid_path(path: &Path) -> io::Result<u64> {
    let c = cstr(path)?;
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: valid C string and writable buffer of the right type.
    check(unsafe { libc::statfs(c.as_ptr(), s.as_mut_ptr()) })?;
    // SAFETY: statfs succeeded and filled `s`.
    let s = unsafe { s.assume_init() };
    Ok(fsid_of(&s))
}

/// Resolve a folder handle (whole `struct file_handle` bytes) to its
/// current path, through the mount open as `mount_fd`.
pub fn handle_path(mount_fd: &OwnedFd, handle: &[u8]) -> io::Result<Vec<u8>> {
    // `struct file_handle` needs 4-byte alignment.
    let mut buf = vec![0u32; handle.len().div_ceil(4)];
    // SAFETY: `buf` has at least `handle.len()` bytes and does not overlap
    // `handle`.
    unsafe {
        std::ptr::copy_nonoverlapping(handle.as_ptr(), buf.as_mut_ptr().cast::<u8>(), handle.len());
    }
    // SAFETY: `buf` holds a file_handle whose `handle_bytes` (checked by the
    // parser to fit) matches its length; the kernel only reads it.
    let fd = check(unsafe {
        libc::open_by_handle_at(
            mount_fd.as_raw_fd(),
            buf.as_mut_ptr().cast::<libc::file_handle>(),
            libc::O_PATH | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: fresh descriptor, closed when dropped.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let p = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))?;
    let b = p.as_os_str().as_bytes();
    if b.first() != Some(&b'/') || b.ends_with(b" (deleted)") {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(b.to_vec())
}

/// `lstat` plus the access ACL, for permission checks.
pub fn meta(path: &Path) -> Option<Meta> {
    let m = std::fs::symlink_metadata(path).ok()?;
    let mut acl = None;
    if m.file_type().is_dir() {
        let c = cstr(path).ok()?;
        let mut buf = [0u8; 4 + 8 * 64];
        // SAFETY: valid C strings and a writable buffer of the given size.
        let n = unsafe {
            libc::lgetxattr(
                c.as_ptr(),
                c"system.posix_acl_access".as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n > 0 {
            // A malformed or oversized ACL denies (no entries match).
            acl = Some(parse_acl(&buf[..n as usize]).unwrap_or_default());
        } else if n < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ERANGE) {
            acl = Some(Vec::new());
        }
    }
    Some(Meta {
        uid: m.uid(),
        gid: m.gid(),
        mode: m.mode(),
        acl,
    })
}

const SO_PEERGROUPS: libc::c_int = 59;

/// The credentials of the process at the other end of a Unix socket, as
/// of `connect()`.
pub fn peer_cred(fd: RawFd) -> io::Result<Cred> {
    let mut uc = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `uc` is a writable ucred and `len` its size.
    check(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut uc).cast(),
            &mut len,
        )
    })?;
    let mut groups = vec![0u32; 64];
    loop {
        let mut len = (groups.len() * 4) as libc::socklen_t;
        // SAFETY: `groups` is a writable buffer of `len` bytes.
        let r = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_PEERGROUPS,
                groups.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r == 0 {
            groups.truncate(len as usize / 4);
            break;
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ERANGE) && (len as usize / 4) > groups.len() {
            groups.resize(len as usize / 4, 0);
            continue;
        }
        return Err(e);
    }
    Ok(Cred {
        uid: uc.uid,
        gid: uc.gid,
        groups,
    })
}

/// `poll(2)` over `fds`; returns the number ready.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: i32) -> io::Result<usize> {
    // SAFETY: `fds` is a valid, writable slice of pollfd.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(0);
        }
        return Err(e);
    }
    Ok(r as usize)
}

/// `read(2)` into `buf`: `Ok(None)` when it would block, `Ok(Some(0))`
/// at end of file.
pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<Option<usize>> {
    loop {
        // SAFETY: `buf` is valid and writable for its length.
        let r = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if r >= 0 {
            return Ok(Some(r as usize));
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => return Ok(None),
            _ => return Err(e),
        }
    }
}

/// The socket systemd passed (fd 3, `SD_LISTEN_FDS_START`). Only call
/// after checking `LISTEN_PID` / `LISTEN_FDS`.
pub fn systemd_listener() -> std::os::unix::net::UnixListener {
    // SAFETY: with socket activation systemd passes the listening socket as
    // fd 3 and nothing else in this process uses that descriptor.
    unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) }
}

/// View a `u64` buffer as bytes (keeps the 8-byte alignment fanotify
/// event records need).
pub fn as_bytes_mut(buf: &mut [u64]) -> &mut [u8] {
    let len = std::mem::size_of_val(buf);
    // SAFETY: u8 has no alignment or validity requirements; the slice
    // covers exactly the same memory and keeps `buf` mutably borrowed.
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), len) }
}

/// Close a descriptor the kernel handed us (event fds).
pub fn close(fd: RawFd) {
    // SAFETY: the caller owns `fd` and never uses it again.
    unsafe {
        libc::close(fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsid_of_path_and_fd_agree() {
        let d = std::env::temp_dir();
        let fd = open_dir(&d).unwrap();
        assert_eq!(fsid_path(&d).unwrap(), fsid_fd(&fd).unwrap());
    }

    #[test]
    fn meta_of_a_folder() {
        let d = tempfile::tempdir().unwrap();
        let m = meta(d.path()).unwrap();
        assert!(m.is_dir());
        assert!(meta(&d.path().join("missing")).is_none());
    }

    #[test]
    fn peer_cred_is_ours() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let c = peer_cred(a.as_raw_fd()).unwrap();
        // SAFETY: getuid cannot fail.
        assert_eq!(c.uid, unsafe { libc::getuid() });
    }
}
