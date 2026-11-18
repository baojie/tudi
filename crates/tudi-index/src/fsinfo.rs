//! What a root lives on (PLAN §5.4, extra roots): filesystem type and
//! device, to pick the reconcile strategy and interval.
//!
//! - Slow device: rotational, or attached over USB (an SD card in a USB
//!   reader reports "rotational" too, so the two cannot be told apart).
//!   Slow roots are reconciled daily by default.
//! - Directory mtimes are trusted for reconcile except on FUSE filesystems,
//!   whose backends may not update them; those roots are re-listed fully.
//!   (Linux's exfat, vfat and ntfs3 drivers update a folder's mtime when an
//!   entry is created, deleted or renamed in it.)
//! - fanotify (M7) needs file handles: not on exFAT, vfat, NTFS or FUSE.

use std::path::{Path, PathBuf};

const EXFAT: i64 = 0x2011_bab0;
const MSDOS: i64 = 0x4d44;
const NTFS3: i64 = 0x7366_746e;
const NTFS: i64 = 0x5346_544e;
const FUSE: i64 = 0x6573_5546;
const EXT4: i64 = 0xef53;
const BTRFS: i64 = 0x9123_683e;
const XFS: i64 = 0x5846_5342;
const TMPFS: i64 = 0x0102_1994;
const F2FS: i64 = 0xf2f5_2010;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootInfo {
    /// Short filesystem name (`exfat`, `ext4`, …, or the hex magic).
    pub fs: String,
    pub rotational: bool,
    pub usb: bool,
    pub removable: bool,
    /// Folder mtimes change on entry create / delete / rename.
    pub mtime_reliable: bool,
    /// fanotify can mark this filesystem (file handles).
    pub fanotify: bool,
}

impl RootInfo {
    /// Rotational or USB: reconcile rarely, never keep the disk busy.
    pub fn slow(&self) -> bool {
        self.rotational || self.usb
    }
}

fn fs_name(magic: i64) -> String {
    match magic {
        EXFAT => "exfat".into(),
        MSDOS => "vfat".into(),
        NTFS3 | NTFS => "ntfs".into(),
        FUSE => "fuse".into(),
        EXT4 => "ext4".into(),
        BTRFS => "btrfs".into(),
        XFS => "xfs".into(),
        TMPFS => "tmpfs".into(),
        F2FS => "f2fs".into(),
        m => format!("{m:#x}"),
    }
}

/// The sysfs directory of the block device holding `dev`, and of the whole
/// disk (for a partition, its parent).
fn sysfs_disk(dev: u64) -> Option<PathBuf> {
    let (major, minor) = (rustix::fs::major(dev), rustix::fs::minor(dev));
    let p = std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok()?;
    if p.join("queue").is_dir() {
        Some(p)
    } else {
        p.parent().map(Path::to_path_buf)
    }
}

/// Detect what `root` lives on. Unknown facts default to "fast, local".
pub fn detect(root: &Path) -> RootInfo {
    let magic = rustix::fs::statfs(root)
        // `f_type`'s integer type differs between architectures.
        .map(
            #[allow(clippy::useless_conversion)]
            |s| i64::from(s.f_type),
        )
        .unwrap_or(0);
    let mut info = RootInfo {
        fs: fs_name(magic),
        mtime_reliable: magic != FUSE,
        fanotify: !matches!(magic, EXFAT | MSDOS | NTFS3 | NTFS | FUSE),
        ..Default::default()
    };
    if let Ok(st) = rustix::fs::stat(root)
        && let Some(disk) = sysfs_disk(st.st_dev)
    {
        let read = |f: &str| std::fs::read_to_string(disk.join(f)).ok();
        info.rotational = read("queue/rotational").is_some_and(|v| v.trim() == "1");
        info.removable = read("removable").is_some_and(|v| v.trim() == "1");
        info.usb = disk.to_string_lossy().contains("/usb");
    }
    info
}

/// Whether a root that should hold files looks unmounted: missing, or an
/// empty folder although the index has entries for it (an unmounted mount
/// point is an empty folder on the parent filesystem).
pub fn looks_offline(root: &Path, indexed_children: usize) -> bool {
    match std::fs::read_dir(root) {
        Err(_) => true,
        Ok(mut it) => indexed_children > 0 && it.next().is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tmp_dir() {
        let t = tempfile::tempdir().unwrap();
        let i = detect(t.path());
        assert!(i.mtime_reliable);
        assert!(!i.fs.is_empty());
        // An empty folder with nothing indexed is online; with indexed
        // entries it looks like an unmounted mount point.
        assert!(!looks_offline(t.path(), 0));
        assert!(looks_offline(t.path(), 5));
        std::fs::write(t.path().join("f"), "").unwrap();
        assert!(!looks_offline(t.path(), 5));
        assert!(looks_offline(&t.path().join("missing"), 0));
    }

    #[test]
    fn names() {
        assert_eq!(fs_name(EXFAT), "exfat");
        assert_eq!(fs_name(0x1234), "0x1234");
    }
}
