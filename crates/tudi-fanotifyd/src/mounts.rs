//! Mounted filesystems (`/proc/self/mountinfo`) and path aliases.
//!
//! The helper marks every mounted filesystem of a type that supports file
//! handles. A folder handle is resolved through one mount of its
//! filesystem (the primary one, preferably mounting the filesystem's own
//! root); the path is then also translated to every other mount of the
//! same filesystem (bind mounts), so a client whose root is reached
//! through a bind mount still matches.

use crate::access::under;

/// Filesystem types the helper marks (they support `FAN_REPORT_DFID_NAME`
/// and `open_by_handle_at`). Not: tmpfs (scratch data), exFAT, vfat, NTFS,
/// FUSE (no stable handles).
pub const SUPPORTED: &[&str] = &["ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "bcachefs"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// Folder inside the filesystem that is mounted (`/` for the whole fs).
    pub root: Vec<u8>,
    /// Where it is mounted.
    pub point: Vec<u8>,
    pub fstype: String,
}

/// Undo mountinfo's octal escapes (`\040` for space, …).
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') as u32 * 64
                + (b[i + 2] - b'0') as u32 * 8
                + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Parse `/proc/self/mountinfo`.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            let fstype = right.split(' ').next()?;
            Some(Mount {
                root: unescape(f.get(3)?),
                point: unescape(f.get(4)?),
                fstype: fstype.to_string(),
            })
        })
        .collect()
}

fn join(base: &[u8], rest: &[u8]) -> Vec<u8> {
    // `rest` is empty or starts with '/'.
    if base == b"/" {
        if rest.is_empty() {
            b"/".to_vec()
        } else {
            rest.to_vec()
        }
    } else {
        let mut v = base.to_vec();
        v.extend_from_slice(rest);
        v
    }
}

fn strip<'a>(path: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if !under(path, prefix) {
        return None;
    }
    Some(if prefix == b"/" {
        path
    } else {
        &path[prefix.len()..]
    })
}

/// `path` (seen through mount `via`) as a path inside the filesystem.
pub fn fs_path(path: &[u8], via: &Mount) -> Option<Vec<u8>> {
    let rest = strip(path, &via.point)?;
    let rest = if rest == b"/" { &b""[..] } else { rest };
    Some(join(&via.root, rest))
}

/// Every path under which the filesystem path `fs` is visible.
pub fn aliases(fs: &[u8], mounts: &[&Mount]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = mounts
        .iter()
        .filter_map(|m| {
            let rest = strip(fs, &m.root)?;
            let rest = if rest == b"/" { &b""[..] } else { rest };
            Some(join(&m.point, rest))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
25 22 0:22 / /tmp rw,nosuid shared:5 - tmpfs tmpfs rw
30 22 8:1 / /data rw,relatime shared:9 - ext4 /dev/sda1 rw
31 22 8:1 /home/shared /srv/my\\040share rw,relatime shared:9 - ext4 /dev/sda1 rw
32 22 8:17 / /extra rw - exfat /dev/sdb1 rw
";

    #[test]
    fn parses_mountinfo_with_escapes() {
        let m = parse_mountinfo(SAMPLE);
        assert_eq!(m.len(), 5);
        assert_eq!(m[0].point, b"/");
        assert_eq!(m[0].fstype, "ext4");
        assert_eq!(m[3].root, b"/home/shared");
        assert_eq!(m[3].point, b"/srv/my share");
        assert_eq!(m[4].fstype, "exfat");
    }

    #[test]
    fn translates_between_bind_mounts() {
        let m = parse_mountinfo(SAMPLE);
        let (data, srv) = (&m[2], &m[3]);
        let fs = fs_path(b"/data/home/shared/docs", data).unwrap();
        assert_eq!(fs, b"/home/shared/docs");
        assert_eq!(
            aliases(&fs, &[data, srv]),
            vec![
                b"/data/home/shared/docs".to_vec(),
                b"/srv/my share/docs".to_vec()
            ]
        );
        // Outside the bind mount's root: only the full mount sees it.
        let fs = fs_path(b"/data/other", data).unwrap();
        assert_eq!(aliases(&fs, &[data, srv]), vec![b"/data/other".to_vec()]);
        // Mount points themselves.
        assert_eq!(fs_path(b"/data", data).unwrap(), b"/");
        assert_eq!(
            aliases(b"/home/shared", &[srv]),
            vec![b"/srv/my share".to_vec()]
        );
        // Root filesystem.
        assert_eq!(fs_path(b"/home/a", &m[0]).unwrap(), b"/home/a");
        assert_eq!(aliases(b"/", &[&m[0]]), vec![b"/".to_vec()]);
        assert!(fs_path(b"/elsewhere", data).is_none());
    }
}
