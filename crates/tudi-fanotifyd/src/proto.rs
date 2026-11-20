//! Wire protocol between `tudi-fanotifyd` and its clients (`tudi-indexd`).
//!
//! Hand-written so the root helper needs no serialisation crates. Every
//! message is a frame: a little-endian `u32` body length, then the body,
//! whose first byte is the message tag.
//!
//! - Client → helper, exactly once: [`ClientMsg::Subscribe`] with the
//!   client's roots. Nothing else is ever accepted from a client.
//! - Helper → client: [`HelperMsg::Covered`] (the filesystems with live
//!   events) once, then any number of [`HelperMsg::Changed`] batches and
//!   [`HelperMsg::Overflow`]; [`HelperMsg::Error`] before closing.
//!
//! Decoding never panics on malformed input; sizes are bounded.

use std::fmt;
use std::io::{self, Read, Write};

/// Where the system unit listens.
pub const SOCKET_PATH: &str = "/run/tudi/fanotify.sock";
/// Protocol version sent in `Subscribe`.
pub const VERSION: u8 = 1;
/// Largest frame a client may send (roots list).
pub const MAX_CLIENT_FRAME: usize = 64 * 1024;
/// Largest frame the helper sends; bigger batches are split.
pub const MAX_HELPER_FRAME: usize = 1024 * 1024;
/// Most roots in one subscription.
pub const MAX_ROOTS: usize = 64;
/// Longest path accepted (Linux `PATH_MAX`).
pub const MAX_PATH: usize = 4096;

const TAG_SUBSCRIBE: u8 = 1;
const TAG_COVERED: u8 = 2;
const TAG_CHANGED: u8 = 3;
const TAG_OVERFLOW: u8 = 4;
const TAG_ERROR: u8 = 5;

/// A filesystem id (`statfs.f_fsid`, the same value fanotify reports).
pub type Fsid = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMsg {
    /// Absolute root paths (bytes) the client wants events for.
    Subscribe { version: u8, roots: Vec<Vec<u8>> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperMsg {
    /// Filesystems the helper marked; roots elsewhere get no events.
    Covered(Vec<Fsid>),
    /// Folders whose entries changed (created, deleted, renamed).
    Changed(Vec<Vec<u8>>),
    /// Events were lost: reconcile.
    Overflow,
    /// The helper refuses the client; the connection closes.
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    TooLarge,
    BadTag(u8),
    BadValue(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated message"),
            Self::TooLarge => write!(f, "message too large"),
            Self::BadTag(t) => write!(f, "unknown message tag {t}"),
            Self::BadValue(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<DecodeError> for io::Error {
    fn from(e: DecodeError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self, max: usize) -> Result<&'a [u8], DecodeError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(DecodeError::TooLarge);
        }
        self.take(n)
    }
    fn end(&self) -> Result<(), DecodeError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::BadValue("trailing bytes"))
        }
    }
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

/// Wrap a body into a frame.
fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

impl ClientMsg {
    pub fn encode(&self) -> Vec<u8> {
        let ClientMsg::Subscribe { version, roots } = self;
        let mut b = vec![TAG_SUBSCRIBE, *version];
        b.extend_from_slice(&(roots.len() as u16).to_le_bytes());
        for r in roots {
            put_bytes(&mut b, r);
        }
        frame(b)
    }

    /// Decode a frame body. Roots must be absolute, without NUL bytes.
    pub fn decode(body: &[u8]) -> Result<Self, DecodeError> {
        if body.len() > MAX_CLIENT_FRAME {
            return Err(DecodeError::TooLarge);
        }
        let mut c = Cur(body);
        match c.u8()? {
            TAG_SUBSCRIBE => {
                let version = c.u8()?;
                let n = c.u16()? as usize;
                if n > MAX_ROOTS {
                    return Err(DecodeError::TooLarge);
                }
                let mut roots = Vec::with_capacity(n);
                for _ in 0..n {
                    let r = c.bytes(MAX_PATH)?;
                    if r.first() != Some(&b'/') || r.contains(&0) {
                        return Err(DecodeError::BadValue("root path"));
                    }
                    roots.push(r.to_vec());
                }
                c.end()?;
                Ok(ClientMsg::Subscribe { version, roots })
            }
            t => Err(DecodeError::BadTag(t)),
        }
    }
}

impl HelperMsg {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            HelperMsg::Covered(ids) => {
                b.push(TAG_COVERED);
                b.extend_from_slice(&(ids.len() as u16).to_le_bytes());
                for id in ids {
                    b.extend_from_slice(&id.to_le_bytes());
                }
            }
            HelperMsg::Changed(dirs) => {
                b.push(TAG_CHANGED);
                b.extend_from_slice(&(dirs.len() as u32).to_le_bytes());
                for d in dirs {
                    put_bytes(&mut b, d);
                }
            }
            HelperMsg::Overflow => b.push(TAG_OVERFLOW),
            HelperMsg::Error(s) => {
                b.push(TAG_ERROR);
                put_bytes(&mut b, s.as_bytes());
            }
        }
        frame(b)
    }

    pub fn decode(body: &[u8]) -> Result<Self, DecodeError> {
        if body.len() > MAX_HELPER_FRAME {
            return Err(DecodeError::TooLarge);
        }
        let mut c = Cur(body);
        let msg = match c.u8()? {
            TAG_COVERED => {
                let n = c.u16()? as usize;
                let mut ids = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    ids.push(c.u64()?);
                }
                HelperMsg::Covered(ids)
            }
            TAG_CHANGED => {
                let n = c.u32()? as usize;
                let mut dirs = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    dirs.push(c.bytes(MAX_PATH)?.to_vec());
                }
                HelperMsg::Changed(dirs)
            }
            TAG_OVERFLOW => HelperMsg::Overflow,
            TAG_ERROR => HelperMsg::Error(String::from_utf8_lossy(c.bytes(MAX_PATH)?).into_owned()),
            t => return Err(DecodeError::BadTag(t)),
        };
        c.end()?;
        Ok(msg)
    }
}

/// Split a list of folders into `Changed` frames below the size limit.
pub fn changed_frames(dirs: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Vec<Vec<u8>> = Vec::new();
    let mut size = 5;
    for d in dirs {
        if size + 4 + d.len() > MAX_HELPER_FRAME && !cur.is_empty() {
            out.push(HelperMsg::Changed(std::mem::take(&mut cur)).encode());
            size = 5;
        }
        size += 4 + d.len();
        cur.push(d.clone());
    }
    if !cur.is_empty() {
        out.push(HelperMsg::Changed(cur).encode());
    }
    out
}

/// Read one frame body (blocking), refusing bodies above `max`.
pub fn read_body(r: &mut impl Read, max: usize) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 || n > max {
        return Err(DecodeError::TooLarge.into());
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Client side: read the next helper message.
pub fn read_helper_msg(r: &mut impl Read) -> io::Result<HelperMsg> {
    Ok(HelperMsg::decode(&read_body(r, MAX_HELPER_FRAME)?)?)
}

/// Client side: send the subscription.
pub fn write_subscribe(w: &mut impl Write, roots: &[Vec<u8>]) -> io::Result<()> {
    w.write_all(
        &ClientMsg::Subscribe {
            version: VERSION,
            roots: roots.to_vec(),
        }
        .encode(),
    )
}

/// Helper side, non-blocking input: if `buf` holds a whole frame, return
/// its body length and the body start offset.
pub fn complete_frame(
    buf: &[u8],
    max: usize,
) -> Result<Option<std::ops::Range<usize>>, DecodeError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let n = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if n == 0 || n > max {
        return Err(DecodeError::TooLarge);
    }
    if buf.len() < 4 + n {
        return Ok(None);
    }
    Ok(Some(4..4 + n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn body(frame: &[u8]) -> &[u8] {
        let r = complete_frame(frame, usize::MAX).unwrap().unwrap();
        &frame[r]
    }

    #[test]
    fn subscribe_round_trip() {
        let m = ClientMsg::Subscribe {
            version: VERSION,
            roots: vec![b"/home/alice".to_vec(), "/data/资料".as_bytes().to_vec()],
        };
        assert_eq!(ClientMsg::decode(body(&m.encode())).unwrap(), m);
    }

    #[test]
    fn helper_round_trip() {
        for m in [
            HelperMsg::Covered(vec![1, u64::MAX]),
            HelperMsg::Changed(vec![b"/a".to_vec(), b"/b/\xff".to_vec()]),
            HelperMsg::Overflow,
            HelperMsg::Error("not a regular user".into()),
        ] {
            assert_eq!(HelperMsg::decode(body(&m.encode())).unwrap(), m);
        }
    }

    #[test]
    fn rejects_relative_or_nul_roots() {
        for bad in [&b"home"[..], b"/a\0b", b""] {
            let m = ClientMsg::Subscribe {
                version: 1,
                roots: vec![bad.to_vec()],
            };
            assert!(ClientMsg::decode(body(&m.encode())).is_err());
        }
    }

    #[test]
    fn rejects_too_many_roots_and_trailing_bytes() {
        let m = ClientMsg::Subscribe {
            version: 1,
            roots: vec![b"/a".to_vec(); MAX_ROOTS + 1],
        };
        assert_eq!(
            ClientMsg::decode(body(&m.encode())),
            Err(DecodeError::TooLarge)
        );
        let mut f = HelperMsg::Overflow.encode();
        f.push(0);
        f[0] = 2;
        assert!(HelperMsg::decode(body(&f)).is_err());
    }

    #[test]
    fn frames_split_large_batches() {
        let dirs: Vec<Vec<u8>> = (0..1000).map(|i| vec![b'a'; 2000 + i]).collect();
        let frames = changed_frames(&dirs);
        assert!(frames.len() > 1);
        let mut back = Vec::new();
        for f in &frames {
            assert!(f.len() - 4 <= MAX_HELPER_FRAME);
            let HelperMsg::Changed(d) = HelperMsg::decode(body(f)).unwrap() else {
                panic!()
            };
            back.extend(d);
        }
        assert_eq!(back, dirs);
    }

    #[test]
    fn partial_frames_wait() {
        let f = HelperMsg::Overflow.encode();
        assert_eq!(complete_frame(&f[..3], 100), Ok(None));
        assert_eq!(complete_frame(&f[..4], 100), Ok(None));
        assert_eq!(complete_frame(&f, 100), Ok(Some(4..5)));
        assert_eq!(
            complete_frame(&[0, 0, 0, 0], 100),
            Err(DecodeError::TooLarge)
        );
    }

    proptest! {
        #[test]
        fn decode_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = ClientMsg::decode(&b);
            let _ = HelperMsg::decode(&b);
        }
    }
}
