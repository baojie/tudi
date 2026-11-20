//! Parsing the fanotify read buffer (`FAN_REPORT_DFID_NAME` mode).
//!
//! Layout (see `fanotify(7)`): a sequence of events, each a
//! `fanotify_event_metadata` (24 bytes) followed by info records up to
//! `event_len`. Each info record starts with a 4-byte header
//! (`info_type`, pad, `len`). For the directory-fid records the header is
//! followed by the 8-byte fsid, then a `struct file_handle`
//! (`handle_bytes`, `handle_type`, bytes) and, for `DFID_NAME`, the
//! NUL-terminated entry name, which the helper ignores: it only reports
//! which folder changed.

pub const FAN_CLOEXEC: u32 = 0x1;
pub const FAN_NONBLOCK: u32 = 0x2;
pub const FAN_CLASS_NOTIF: u32 = 0x0;
pub const FAN_REPORT_DIR_FID: u32 = 0x400;
pub const FAN_REPORT_NAME: u32 = 0x800;
pub const FAN_REPORT_DFID_NAME: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;

pub const FAN_MARK_ADD: u32 = 0x1;
pub const FAN_MARK_FILESYSTEM: u32 = 0x100;

pub const FAN_MOVED_FROM: u64 = 0x40;
pub const FAN_MOVED_TO: u64 = 0x80;
pub const FAN_CREATE: u64 = 0x100;
pub const FAN_DELETE: u64 = 0x200;
pub const FAN_Q_OVERFLOW: u64 = 0x4000;
pub const FAN_ONDIR: u64 = 0x4000_0000;

/// What the helper listens for: entries appearing in or leaving a folder.
/// `FAN_DELETE_SELF` / `FAN_MOVE_SELF` add nothing here: the parent folder
/// gets `FAN_DELETE` / `FAN_MOVED_*` for the same change.
pub const MASK: u64 = FAN_CREATE | FAN_DELETE | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_ONDIR;

const METADATA_LEN: usize = 24;
const METADATA_VERSION: u8 = 3;
const INFO_FID: u8 = 1;
const INFO_DFID_NAME: u8 = 2;
const INFO_DFID: u8 = 3;
const INFO_OLD_DFID_NAME: u8 = 10;
const INFO_NEW_DFID_NAME: u8 = 12;

/// A folder that changed: its filesystem and its file handle (the whole
/// `struct file_handle`, header included, as `open_by_handle_at` wants).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DirFid {
    pub fsid: u64,
    pub handle: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Overflow,
    Changed(DirFid),
    /// An event fd the caller must close (not expected in FID mode).
    Fd(i32),
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(b.get(i..i + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// Parse one read's worth of events. Malformed tails are dropped.
pub fn parse(buf: &[u8], out: &mut Vec<Event>) {
    let mut off = 0;
    while off + METADATA_LEN <= buf.len() {
        let ev = &buf[off..];
        let Some(len) = u32_at(ev, 0).map(|l| l as usize) else {
            return;
        };
        if len < METADATA_LEN || len > ev.len() || ev[4] != METADATA_VERSION {
            return;
        }
        let ev = &ev[..len];
        let meta_len = u16_at(ev, 6).unwrap_or(0) as usize;
        let mask = u64_at(ev, 8).unwrap_or(0);
        let fd = u32_at(ev, 16).unwrap_or(u32::MAX) as i32;
        if fd >= 0 {
            out.push(Event::Fd(fd));
        }
        if mask & FAN_Q_OVERFLOW != 0 {
            out.push(Event::Overflow);
        } else if mask & MASK != 0 {
            let mut i = meta_len.max(METADATA_LEN);
            while i + 4 <= ev.len() {
                let kind = ev[i];
                let rec_len = u16_at(ev, i + 2).unwrap_or(0) as usize;
                if rec_len < 4 || i + rec_len > ev.len() {
                    break;
                }
                let rec = &ev[i..i + rec_len];
                if matches!(
                    kind,
                    INFO_DFID_NAME | INFO_DFID | INFO_OLD_DFID_NAME | INFO_NEW_DFID_NAME | INFO_FID
                ) && let Some(f) = parse_fid(rec)
                {
                    out.push(Event::Changed(f));
                    break; // one folder per event
                }
                i += rec_len;
            }
        }
        off += len;
    }
}

fn parse_fid(rec: &[u8]) -> Option<DirFid> {
    let fsid = u64_at(rec, 4)?;
    let hbytes = u32_at(rec, 12)? as usize;
    if hbytes == 0 || hbytes > 128 {
        return None; // MAX_HANDLE_SZ
    }
    let handle = rec.get(12..12 + 8 + hbytes)?.to_vec();
    Some(DirFid { fsid, handle })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build one event as the kernel would.
    pub(crate) fn event(mask: u64, fsid: u64, handle: &[u8], name: &[u8]) -> Vec<u8> {
        let mut info = vec![INFO_DFID_NAME, 0, 0, 0];
        info.extend_from_slice(&fsid.to_ne_bytes());
        info.extend_from_slice(&(handle.len() as u32).to_ne_bytes());
        info.extend_from_slice(&1i32.to_ne_bytes()); // handle_type
        info.extend_from_slice(handle);
        info.extend_from_slice(name);
        info.push(0);
        while !info.len().is_multiple_of(4) {
            info.push(0);
        }
        let ilen = info.len() as u16;
        info[2..4].copy_from_slice(&ilen.to_ne_bytes());
        let total = (METADATA_LEN + info.len()) as u32;
        let mut ev = Vec::new();
        ev.extend_from_slice(&total.to_ne_bytes());
        ev.push(METADATA_VERSION);
        ev.push(0);
        ev.extend_from_slice(&(METADATA_LEN as u16).to_ne_bytes());
        ev.extend_from_slice(&mask.to_ne_bytes());
        ev.extend_from_slice(&(-1i32).to_ne_bytes());
        ev.extend_from_slice(&42i32.to_ne_bytes());
        ev.extend_from_slice(&info);
        ev
    }

    fn overflow() -> Vec<u8> {
        let mut ev = Vec::new();
        ev.extend_from_slice(&(METADATA_LEN as u32).to_ne_bytes());
        ev.push(METADATA_VERSION);
        ev.push(0);
        ev.extend_from_slice(&(METADATA_LEN as u16).to_ne_bytes());
        ev.extend_from_slice(&FAN_Q_OVERFLOW.to_ne_bytes());
        ev.extend_from_slice(&(-1i32).to_ne_bytes());
        ev.extend_from_slice(&0i32.to_ne_bytes());
        ev
    }

    #[test]
    fn parses_events_and_overflow() {
        let mut buf = event(FAN_CREATE, 7, &[1, 2, 3, 4, 5, 6, 7, 8], b"new.txt");
        buf.extend(event(
            FAN_DELETE | FAN_ONDIR,
            9,
            &[9; 12],
            "目录".as_bytes(),
        ));
        buf.extend(overflow());
        let mut out = Vec::new();
        parse(&buf, &mut out);
        assert_eq!(out.len(), 3);
        let Event::Changed(f) = &out[0] else { panic!() };
        assert_eq!(f.fsid, 7);
        assert_eq!(f.handle.len(), 16);
        assert_eq!(&f.handle[8..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        let Event::Changed(g) = &out[1] else { panic!() };
        assert_eq!(g.fsid, 9);
        assert_eq!(out[2], Event::Overflow);
    }

    #[test]
    fn same_folder_same_key() {
        let mut out = Vec::new();
        parse(&event(FAN_CREATE, 1, &[5; 8], b"a"), &mut out);
        parse(&event(FAN_MOVED_TO, 1, &[5; 8], b"b"), &mut out);
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn truncated_or_garbage_input_is_dropped() {
        let ev = event(FAN_CREATE, 1, &[5; 8], b"a");
        for cut in 0..ev.len() {
            let mut out = Vec::new();
            parse(&ev[..cut], &mut out);
            assert!(out.is_empty(), "cut at {cut}");
        }
        let mut out = Vec::new();
        parse(&[0xff; 100], &mut out);
        assert!(out.is_empty());
    }
}
