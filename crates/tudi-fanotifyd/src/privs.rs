//! Dropping privileges after setup.
//!
//! Setup (fanotify group, filesystem marks, socket) needs root /
//! `CAP_SYS_ADMIN`. Afterwards the helper only needs
//! `CAP_DAC_READ_SEARCH`: for `open_by_handle_at` and to `lstat` folders on
//! the way to a changed folder. So it
//!
//! 1. drops every other capability from the bounding set,
//! 2. switches to an unprivileged user (default `nobody`) with no
//!    supplementary groups, keeping its permitted capabilities,
//! 3. sets permitted = effective = {`CAP_DAC_READ_SEARCH`}, inheritable and
//!    ambient empty,
//! 4. sets `PR_SET_NO_NEW_PRIVS`,
//!
//! and then verifies the result from `/proc/self/status`.

use std::ffi::CString;
use std::io;

pub const CAP_DAC_READ_SEARCH: u32 = 2;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_ulong = 4;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn check(r: libc::c_long) -> io::Result<()> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn prctl(op: libc::c_int, arg: libc::c_ulong) -> io::Result<()> {
    // SAFETY: prctl with integer arguments only.
    check(
        unsafe {
            libc::prctl(
                op,
                arg,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        }
        .into(),
    )
}

fn last_cap() -> u32 {
    std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(63)
}

/// uid and gid of `name` from the password database.
fn lookup_user(name: &str) -> Option<(u32, u32)> {
    let c = CString::new(name).ok()?;
    // SAFETY: getpwnam returns a pointer to static storage or null; the
    // helper is single-threaded at this point, and the fields are copied
    // out at once.
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    // SAFETY: non-null result of getpwnam.
    let pw = unsafe { &*pw };
    Some((pw.pw_uid, pw.pw_gid))
}

/// Drop to `user` (when running as root) with only `CAP_DAC_READ_SEARCH`.
pub fn drop_privileges(user: &str) -> io::Result<()> {
    for cap in 0..=last_cap() {
        if cap != CAP_DAC_READ_SEARCH {
            match prctl(libc::PR_CAPBSET_DROP, cap.into()) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {}
                Err(e) => return Err(e),
            }
        }
    }
    // SAFETY: geteuid cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        let (uid, gid) = lookup_user(user).unwrap_or((65534, 65534));
        if uid == 0 {
            return Err(io::Error::other("refusing to run as root after setup"));
        }
        prctl(libc::PR_SET_KEEPCAPS, 1)?;
        // SAFETY: setgroups with an empty list.
        check(unsafe { libc::setgroups(0, std::ptr::null()) }.into())?;
        // SAFETY: integer arguments only.
        check(unsafe { libc::setresgid(gid, gid, gid) }.into())?;
        // SAFETY: integer arguments only.
        check(unsafe { libc::setresuid(uid, uid, uid) }.into())?;
        prctl(libc::PR_SET_KEEPCAPS, 0)?;
    }
    let keep = 1u32 << CAP_DAC_READ_SEARCH;
    let mut hdr = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let data = [
        CapData {
            effective: keep,
            permitted: keep,
            inheritable: 0,
        },
        CapData::default(),
    ];
    // SAFETY: capset(2) with a v3 header and two data structs, as the ABI
    // requires; the kernel only reads them.
    check(unsafe { libc::syscall(libc::SYS_capset, &raw mut hdr, data.as_ptr()) })?;
    // SAFETY: prctl with integer arguments only.
    let _ = check(
        unsafe {
            libc::prctl(
                PR_CAP_AMBIENT,
                PR_CAP_AMBIENT_CLEAR_ALL,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        }
        .into(),
    );
    prctl(libc::PR_SET_NO_NEW_PRIVS, 1)?;
    verify()
}

/// Read back uid and capability sets; error if anything beyond
/// `CAP_DAC_READ_SEARCH` is left.
pub fn verify() -> io::Result<()> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let hex = |name: &str| u64::from_str_radix(&field(name), 16).unwrap_or(u64::MAX);
    let keep = 1u64 << CAP_DAC_READ_SEARCH;
    let uids = field("Uid:");
    let ok = hex("CapEff:") & !keep == 0
        && hex("CapPrm:") & !keep == 0
        && hex("CapInh:") == 0
        && hex("CapBnd:") & !keep == 0
        && hex("CapAmb:") == 0
        && !uids.split_whitespace().any(|u| u == "0");
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "privileges not dropped: Uid {uids}, CapEff {}, CapPrm {}, CapBnd {}",
            field("CapEff:"),
            field("CapPrm:"),
            field("CapBnd:")
        )))
    }
}
