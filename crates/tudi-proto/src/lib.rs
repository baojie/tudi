//! Messages between tudi clients (GUI, `tudi q`) and the index daemon
//! (PLAN §5.4 IPC): a Unix socket at `$XDG_RUNTIME_DIR/tudi/indexd.sock`,
//! frames of a little-endian `u32` length followed by a `postcard`-encoded
//! message. One request, one response, in order.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Frames larger than this are refused.
pub const MAX_FRAME: u32 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Query {
        id: u64,
        text: String,
        limit: u32,
        show_hidden: bool,
        /// Only folders (for `tudi q --dir` / jumping).
        dirs_only: bool,
        /// Only items below this folder (absolute path bytes).
        scope: Option<Vec<u8>>,
    },
    Status,
    /// Rescan everything from scratch.
    Rescan,
    /// Save the snapshot and exit.
    Stop,
    /// A client just listed a folder: free accuracy for the index.
    ListingHint {
        dir: Vec<u8>,
        entries: Vec<HintEntry>,
    },
    /// The frecency store changed; reload weights.
    FrecencyChanged,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HintEntry {
    pub name: Vec<u8>,
    /// 0 file, 1 folder, 2 symlink, 3 other.
    pub kind: u8,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultItem {
    /// Absolute path bytes.
    pub path: Vec<u8>,
    pub kind: u8,
    pub size: u64,
    pub mtime: i64,
    pub score: f32,
    /// Matched character positions in the file name, for highlighting.
    pub indices: Vec<u32>,
}

impl ResultItem {
    pub fn path(&self) -> PathBuf {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(self.path.clone()))
    }

    pub fn is_dir(&self) -> bool {
        self.kind == 1
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub entries: u64,
    pub roots: Vec<String>,
    /// "loading", "scanning", "reconciling", "ready".
    pub state: String,
    /// Unix seconds of the last completed scan or reconcile.
    pub last_scan: i64,
    pub rss_mb: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Results {
        id: u64,
        items: Vec<ResultItem>,
        elapsed_us: u32,
        /// The index is still being built: results may be incomplete.
        partial: bool,
        /// Problems with the query (English), e.g. a bad date filter.
        hints: Vec<String>,
    },
    Status(Status),
    Ok,
    Error(String),
}

/// `$XDG_RUNTIME_DIR/tudi/indexd.sock` (falls back to `/tmp/tudi-$UID`).
pub fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/tudi-{}", rustix_uid())));
    base.join("tudi/indexd.sock")
}

fn rustix_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map_or(0, |m| m.uid())
}

pub fn write_frame<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&l| l <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn read_frame<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A blocking client connection.
pub struct Client {
    stream: std::os::unix::net::UnixStream,
}

impl Client {
    pub fn connect() -> io::Result<Self> {
        Self::connect_to(&socket_path())
    }

    pub fn connect_to(path: &std::path::Path) -> io::Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
        Ok(Self { stream })
    }

    pub fn call(&mut self, req: &Request) -> io::Result<Response> {
        write_frame(&mut self.stream, req)?;
        read_frame(&mut self.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let req = Request::Query {
            id: 7,
            text: "acme 季报".into(),
            limit: 50,
            show_hidden: false,
            dirs_only: false,
            scope: Some("/home/u/文档".as_bytes().to_vec()),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        write_frame(&mut buf, &Request::Status).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame::<Request>(&mut r).unwrap(), req);
        assert_eq!(read_frame::<Request>(&mut r).unwrap(), Request::Status);
        assert!(read_frame::<Request>(&mut r).is_err(), "end of stream");
    }

    #[test]
    fn oversized_frame_is_refused() {
        let mut buf = (MAX_FRAME + 1).to_le_bytes().to_vec();
        buf.extend_from_slice(&[0; 8]);
        assert!(read_frame::<Request>(&mut &buf[..]).is_err());
    }

    #[test]
    fn client_server_over_a_socket() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let req: Request = read_frame(&mut s).unwrap();
            assert_eq!(req, Request::Status);
            write_frame(
                &mut s,
                &Response::Status(Status {
                    entries: 3,
                    ..Default::default()
                }),
            )
            .unwrap();
        });
        let mut c = Client::connect_to(&path).unwrap();
        match c.call(&Request::Status).unwrap() {
            Response::Status(s) => assert_eq!(s.entries, 3),
            r => panic!("{r:?}"),
        }
        server.join().unwrap();
    }
}
