//! Which changed folders a client may hear about.
//!
//! A folder `D` is forwarded to a client with credentials `C` only if:
//!
//! 1. `D` is one of the client's roots `R` or below it, and
//! 2. `C` may search (`x`) every folder above `R`, and
//! 3. `C` may list (`r` and `x`) every folder from `R` down to `D`.
//!
//! So a client learns only names it could read itself by listing from its
//! own roots: a folder's path is revealed only if every folder on the way
//! is readable. A root the client names but cannot reach yields nothing.
//!
//! Permissions are evaluated here from `lstat` and the POSIX access ACL
//! (`system.posix_acl_access`), following the kernel's
//! `posix_acl_permission`. This keeps the helper from needing
//! `CAP_SETUID` to check access with the client's credentials. Linux
//! security modules (SELinux, AppArmor) are not evaluated: a client may be
//! told about a folder an LSM would hide from it, never about one that
//! plain permissions hide.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub const R: u16 = 4;
pub const X: u16 = 1;

/// A client's credentials (from `SO_PEERCRED` / `SO_PEERGROUPS`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cred {
    pub uid: u32,
    pub gid: u32,
    /// Supplementary groups.
    pub groups: Vec<u32>,
}

impl Cred {
    fn in_group(&self, g: u32) -> bool {
        self.gid == g || self.groups.contains(&g)
    }
}

const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;
const ACL_XATTR_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AclEntry {
    pub tag: u16,
    pub perm: u16,
    pub id: u32,
}

/// What permission checks need to know about a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub uid: u32,
    pub gid: u32,
    /// `st_mode` (type and permission bits).
    pub mode: u32,
    /// The access ACL, if the folder has an extended one.
    pub acl: Option<Vec<AclEntry>>,
}

impl Meta {
    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
}

/// Parse a `system.posix_acl_access` value. `None` if malformed.
pub fn parse_acl(b: &[u8]) -> Option<Vec<AclEntry>> {
    if b.len() < 4 || !(b.len() - 4).is_multiple_of(8) {
        return None;
    }
    if u32::from_le_bytes(b[..4].try_into().ok()?) != ACL_XATTR_VERSION {
        return None;
    }
    Some(
        b[4..]
            .chunks_exact(8)
            .map(|e| AclEntry {
                tag: u16::from_le_bytes([e[0], e[1]]),
                perm: u16::from_le_bytes([e[2], e[3]]),
                id: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            })
            .collect(),
    )
}

/// Does `c` have all of `want` (`R`/`X` bits) on a file with `m`?
pub fn permits(m: &Meta, c: &Cred, want: u16) -> bool {
    match &m.acl {
        Some(acl) => acl_permits(acl, m, c, want),
        None => {
            let bits = if m.uid == c.uid {
                m.mode >> 6
            } else if c.in_group(m.gid) {
                m.mode >> 3
            } else {
                m.mode
            } & 7;
            bits as u16 & want == want
        }
    }
}

/// The kernel's `posix_acl_permission`, entries in canonical order.
fn acl_permits(acl: &[AclEntry], m: &Meta, c: &Cred, want: u16) -> bool {
    let mask = acl.iter().find(|e| e.tag == ACL_MASK).map(|e| e.perm);
    let masked = |perm: u16| perm & mask.unwrap_or(7) & want == want;
    let mut found_group = false;
    for e in acl {
        match e.tag {
            ACL_USER_OBJ if m.uid == c.uid => return e.perm & want == want,
            ACL_USER if e.id == c.uid => return masked(e.perm),
            ACL_GROUP_OBJ if c.in_group(m.gid) => {
                found_group = true;
                if e.perm & want == want {
                    return masked(e.perm);
                }
            }
            ACL_GROUP if c.in_group(e.id) => {
                found_group = true;
                if e.perm & want == want {
                    return masked(e.perm);
                }
            }
            ACL_OTHER => return !found_group && e.perm & want == want,
            _ => {}
        }
    }
    false
}

/// Normalise a root: no trailing slash (except `/`).
pub fn normalise_root(r: &[u8]) -> Vec<u8> {
    let mut v = r.to_vec();
    while v.len() > 1 && v.last() == Some(&b'/') {
        v.pop();
    }
    v
}

/// Is `path` the folder `root` or below it?
pub fn under(path: &[u8], root: &[u8]) -> bool {
    root == b"/"
        || path == root
        || (path.len() > root.len() && path.starts_with(root) && path[root.len()] == b'/')
}

/// The folders from `/` down to `path`, each with the permission needed.
fn steps<'a>(path: &'a [u8], root: &[u8]) -> impl Iterator<Item = (&'a [u8], u16)> {
    let ends = std::iter::once(1usize)
        .chain(
            path.iter()
                .enumerate()
                .skip(1)
                .filter(|&(_, &b)| b == b'/')
                .map(|(i, _)| i),
        )
        .chain(std::iter::once(path.len()));
    let root_len = root.len();
    let mut last = 0;
    ends.filter_map(move |e| {
        if e == last || e > path.len() {
            return None;
        }
        last = e;
        let p = &path[..e];
        Some((p, if p.len() < root_len { X } else { R | X }))
    })
}

/// Per-batch cache of folder metadata.
pub struct MetaCache<F: FnMut(&Path) -> Option<Meta>> {
    lookup: F,
    seen: HashMap<PathBuf, Option<Meta>>,
}

impl<F: FnMut(&Path) -> Option<Meta>> MetaCache<F> {
    pub fn new(lookup: F) -> Self {
        Self {
            lookup,
            seen: HashMap::new(),
        }
    }

    fn get(&mut self, p: &[u8]) -> Option<&Meta> {
        let path = Path::new(OsStr::from_bytes(p));
        if !self.seen.contains_key(path) {
            let m = (self.lookup)(path);
            self.seen.insert(path.to_path_buf(), m);
        }
        self.seen.get(path).and_then(Option::as_ref)
    }

    /// May `cred`, subscribed to `roots` (normalised), hear about `dir`?
    pub fn visible(&mut self, dir: &[u8], roots: &[Vec<u8>], cred: &Cred) -> bool {
        roots.iter().filter(|r| under(dir, r)).any(|r| {
            steps(dir, r).all(|(p, want)| {
                self.get(p)
                    .is_some_and(|m| m.is_dir() && permits(m, cred, want))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(uid: u32, gid: u32, groups: &[u32]) -> Cred {
        Cred {
            uid,
            gid,
            groups: groups.to_vec(),
        }
    }

    fn dir(uid: u32, gid: u32, perm: u32) -> Meta {
        Meta {
            uid,
            gid,
            mode: 0o040000 | perm,
            acl: None,
        }
    }

    fn acl(entries: &[(u16, u16, u32)]) -> Vec<AclEntry> {
        entries
            .iter()
            .map(|&(tag, perm, id)| AclEntry { tag, perm, id })
            .collect()
    }

    #[test]
    fn mode_bits_owner_group_other() {
        let alice = cred(1001, 1001, &[50]);
        assert!(permits(&dir(1001, 0, 0o700), &alice, R | X));
        assert!(!permits(&dir(0, 0, 0o700), &alice, X));
        assert!(permits(&dir(0, 50, 0o750), &alice, R | X));
        assert!(permits(&dir(0, 0, 0o711), &alice, X));
        assert!(!permits(&dir(0, 0, 0o711), &alice, R | X));
        // Owner class wins even when other bits are wider.
        assert!(!permits(&dir(1001, 0, 0o077), &alice, X));
        // Group class likewise.
        assert!(!permits(&dir(0, 50, 0o707), &alice, X));
    }

    #[test]
    fn acl_named_user_with_mask() {
        let alice = cred(1001, 1001, &[]);
        let mut m = dir(0, 0, 0o750);
        m.acl = Some(acl(&[
            (ACL_USER_OBJ, 7, 0),
            (ACL_USER, 5, 1001),
            (ACL_GROUP_OBJ, 5, 0),
            (ACL_MASK, 5, 0),
            (ACL_OTHER, 0, 0),
        ]));
        assert!(permits(&m, &alice, R | X));
        // The mask limits named users.
        m.acl.as_mut().unwrap()[3].perm = 1;
        assert!(!permits(&m, &alice, R | X));
        assert!(permits(&m, &alice, X));
    }

    #[test]
    fn acl_matching_group_denies_instead_of_other() {
        let bob = cred(1002, 1002, &[60]);
        let mut m = dir(0, 0, 0o705);
        m.acl = Some(acl(&[
            (ACL_USER_OBJ, 7, 0),
            (ACL_GROUP_OBJ, 0, 0),
            (ACL_GROUP, 0, 60),
            (ACL_MASK, 7, 0),
            (ACL_OTHER, 5, 0),
        ]));
        // Bob is in group 60, which has no access: "other" does not apply.
        assert!(!permits(&m, &bob, X));
        assert!(permits(&m, &cred(1003, 1003, &[]), R | X));
    }

    #[test]
    fn acl_xattr_parsing() {
        let mut b = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [(1u16, 7u16, u32::MAX), (2, 5, 1001), (32, 0, u32::MAX)] {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&perm.to_le_bytes());
            b.extend_from_slice(&id.to_le_bytes());
        }
        let a = parse_acl(&b).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(
            a[1],
            AclEntry {
                tag: 2,
                perm: 5,
                id: 1001
            }
        );
        assert!(parse_acl(&b[..7]).is_none());
        assert!(parse_acl(&1u32.to_le_bytes()).is_none());
    }

    #[test]
    fn under_roots() {
        assert!(under(b"/home/a", b"/home/a"));
        assert!(under(b"/home/a/x", b"/home/a"));
        assert!(!under(b"/home/ab", b"/home/a"));
        assert!(!under(b"/home", b"/home/a"));
        assert!(under(b"/anything", b"/"));
        assert_eq!(normalise_root(b"/home/a//"), b"/home/a");
        assert_eq!(normalise_root(b"/"), b"/");
    }

    #[test]
    fn steps_need_x_above_root_and_rx_below() {
        let s: Vec<(&[u8], u16)> = steps(b"/home/a/x/y", b"/home/a").collect();
        assert_eq!(
            s,
            vec![
                (&b"/"[..], X),
                (b"/home", X),
                (b"/home/a", R | X),
                (b"/home/a/x", R | X),
                (b"/home/a/x/y", R | X),
            ]
        );
        let s: Vec<(&[u8], u16)> = steps(b"/", b"/").collect();
        assert_eq!(s, vec![(&b"/"[..], R | X)]);
    }

    /// A fake tree: /home (755 root), /home/a (700 alice),
    /// /home/a/pub (755 alice), /home/b (711 bob), /home/b/pub (755 bob),
    /// /home/b/pub/deep (755 bob).
    fn tree(p: &Path) -> Option<Meta> {
        Some(match p.to_str()? {
            "/" | "/home" => dir(0, 0, 0o755),
            "/home/a" => dir(1001, 1001, 0o700),
            "/home/a/pub" => dir(1001, 1001, 0o755),
            "/home/b" => dir(1002, 1002, 0o711),
            "/home/b/pub" | "/home/b/pub/deep" => dir(1002, 1002, 0o755),
            "/home/b/file" => Meta {
                uid: 1002,
                gid: 1002,
                mode: 0o100644,
                acl: None,
            },
            _ => return None,
        })
    }

    #[test]
    fn visibility_rules() {
        let alice = cred(1001, 1001, &[]);
        let bob = cred(1002, 1002, &[]);
        let mut c = MetaCache::new(tree);
        let ra = vec![b"/home/a".to_vec()];
        assert!(c.visible(b"/home/a/pub", &ra, &alice));
        // Not under alice's roots.
        assert!(!c.visible(b"/home/b/pub", &ra, &alice));
        // Bob cannot enter /home/a.
        assert!(!c.visible(b"/home/a/pub", &ra, &bob));
        // Alice names bob's home: /home/b is 711, so she cannot list it and
        // must not learn the names inside.
        let rb = vec![b"/home/b".to_vec()];
        assert!(!c.visible(b"/home/b/pub", &rb, &alice));
        // ...but if she already knows /home/b/pub (her root), its contents
        // are hers to see: she can reach and list it.
        let rbp = vec![b"/home/b/pub".to_vec()];
        assert!(c.visible(b"/home/b/pub/deep", &rbp, &alice));
        // Root "/" cannot be used to see past an unreadable folder.
        let all = vec![b"/".to_vec()];
        assert!(!c.visible(b"/home/b/pub", &all, &alice));
        assert!(c.visible(b"/home/b/pub", &all, &bob));
        // Missing folders and non-folders are never visible.
        assert!(!c.visible(b"/home/a/gone", &ra, &alice));
        assert!(!c.visible(b"/home/b/file", &rb, &bob));
    }
}
