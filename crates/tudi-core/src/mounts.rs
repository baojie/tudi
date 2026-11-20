//! Mounted disks for the folder tree (`/proc/self/mountinfo`).
//!
//! GIO's volume monitor hides mounts it considers "system" ones, which
//! includes every fstab mount outside `/media` and `/run/media` (a second
//! disk at `/data`, say). The tree shows every mounted disk, so the GUI also
//! reads mountinfo itself: [`disks`] picks the mounts backed by a disk and
//! [`MountWatcher`] re-reads the table whenever the kernel reports a change.

use std::ffi::OsString;
use std::io::{self, Read, Seek};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use rustix::event::{PollFd, PollFlags, poll};

/// A mounted disk shown as a tree root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// Mount point.
    pub path: PathBuf,
    /// Label: the mount point's last component.
    pub name: String,
}

/// Filesystem types that live on a disk (local or removable).
const DISK_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "bcachefs", "f2fs", "jfs", "reiserfs", "zfs", "vfat",
    "exfat", "ntfs", "ntfs3", "fuseblk", "hfs", "hfsplus", "apfs", "iso9660", "udf",
];

/// Mount points under these are system plumbing, never shown.
const SYSTEM_PREFIXES: &[&str] = &[
    "/boot", "/snap", "/var", "/usr", "/etc", "/proc", "/sys", "/dev",
];

/// Undo mountinfo's octal escapes (`\040` for space, …).
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let oct = b
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
        if b[i] == b'\\'
            && let Some(d) = oct
        {
            let v = d.iter().fold(0u32, |a, c| a * 8 + u32::from(c - b'0'));
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn under(path: &Path, prefix: &str) -> bool {
    path.starts_with(prefix)
}

/// Whether a mount point is plumbing rather than a disk the user reaches.
fn is_system(path: &Path) -> bool {
    if path == Path::new("/") {
        return true;
    }
    if SYSTEM_PREFIXES.iter().any(|p| under(path, p)) {
        return true;
    }
    // `/run` and `/tmp` hold runtime mounts; removable media under
    // `/run/media` are the exception.
    (under(path, "/run") && !under(path, "/run/media")) || under(path, "/tmp")
}

/// The disks in a mountinfo table, in table order: mounts of a disk
/// filesystem whose mount point is not system plumbing. A filesystem
/// mounted more than once (bind mounts) is listed at its first mount of the
/// filesystem's root only.
pub fn disks(mountinfo: &str) -> Vec<Disk> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in mountinfo.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        // id parent major:minor root mount-point options [optional…] - fstype source super
        let Some(dash) = f.iter().position(|x| *x == "-") else {
            continue;
        };
        if f.len() < dash + 3 || dash < 5 {
            continue;
        }
        let (dev, root, point, fstype) = (f[2], f[3], f[4], f[dash + 1]);
        if !DISK_FS.contains(&fstype) {
            continue;
        }
        // A bind mount of a folder inside a filesystem; btrfs subvolumes
        // (`/@data`) are whole volumes to the user and stay.
        if root != "/" && fstype != "btrfs" {
            continue;
        }
        let path = PathBuf::from(OsString::from_vec(unescape(point)));
        if is_system(&path) {
            continue;
        }
        let key = format!("{dev} {root}");
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        out.push(Disk { path, name });
    }
    out
}

/// Blocks on `/proc/self/mountinfo` until the mount table changes. Use from
/// a worker thread.
pub struct MountWatcher {
    file: std::fs::File,
}

impl MountWatcher {
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            file: std::fs::File::open("/proc/self/mountinfo")?,
        })
    }

    /// The current table.
    pub fn read(&mut self) -> io::Result<String> {
        self.file.rewind()?;
        let mut s = String::new();
        self.file.read_to_string(&mut s)?;
        Ok(s)
    }

    /// Wait until the table changes (the kernel flags mountinfo with
    /// `POLLPRI`/`POLLERR`), then return it.
    pub fn wait(&mut self) -> io::Result<String> {
        let mut fds = [PollFd::new(&self.file, PollFlags::PRI | PollFlags::ERR)];
        loop {
            match poll(&mut fds, None) {
                Ok(_) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        self.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
29 1 259:5 / / rw,relatime shared:1 - ext4 /dev/nvme1n1p5 rw
30 29 259:1 / /boot/efi rw shared:2 - vfat /dev/nvme1n1p1 rw
31 29 7:0 / /snap/core/17292 ro shared:3 - squashfs /dev/loop0 ro
32 29 0:30 / /tmp rw shared:4 - tmpfs tmpfs rw
33 29 8:17 / /extra rw shared:5 - exfat /dev/sdb1 rw
34 29 259:2 / /data rw shared:6 - ext4 /dev/nvme0n1p2 rw
35 34 8:1 / /data/quip rw shared:7 - fuseblk /dev/sda1 rw
36 29 259:3 / /run/media/user/Windows-SSD rw shared:8 - ntfs3 /dev/nvme1n1p3 rw
37 29 179:1 / /run/media/user/Ubuntu\\04024.04 ro shared:9 - iso9660 /dev/mmcblk0p1 ro
38 29 259:2 /projects /srv/projects rw shared:6 - ext4 /dev/nvme0n1p2 rw
39 29 0:45 / /run/user/1000/gvfs rw shared:10 - fuse.gvfsd-fuse gvfsd-fuse rw
40 29 0:50 /@home /home rw shared:11 - btrfs /dev/sdc1 rw
41 29 259:2 / /mnt/data-again rw shared:6 - ext4 /dev/nvme0n1p2 rw
42 29 0:60 / /var/lib/docker rw shared:12 - ext4 /dev/sdd1 rw
";

    #[test]
    fn picks_disks_only() {
        let d = disks(SAMPLE);
        let paths: Vec<&str> = d.iter().map(|d| d.path.to_str().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "/extra",
                "/data",
                "/data/quip",
                "/run/media/user/Windows-SSD",
                "/run/media/user/Ubuntu 24.04",
                "/home",
            ]
        );
        assert_eq!(d[0].name, "extra");
        assert_eq!(d[4].name, "Ubuntu 24.04");
    }

    #[test]
    fn non_utf8_mount_point_is_kept_as_bytes() {
        let t = "1 0 8:1 / /mnt/\\377x rw - ext4 /dev/sda1 rw\n";
        let d = disks(t);
        assert_eq!(d[0].path.as_os_str().as_encoded_bytes(), b"/mnt/\xffx");
    }

    #[test]
    fn malformed_lines_are_skipped() {
        assert!(disks("garbage\n1 2 3 - ext4\n").is_empty());
    }

    #[test]
    fn watcher_reads_own_table() {
        let mut w = MountWatcher::open().unwrap();
        assert!(w.read().unwrap().contains(" / "));
    }
}
