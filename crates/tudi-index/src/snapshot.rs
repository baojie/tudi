//! Index snapshots (PLAN §5.4): a versioned binary file with a checksum,
//! written atomically (temp file, fsync, rename), so a crash mid-write
//! leaves the previous snapshot intact.
//!
//! Layout (little endian):
//!
//! ```text
//! magic "TUDIIDX\0" | version u32 | node_count u64 | names_len u64 | roots u32
//! roots × (path_len u32, path bytes, node u32)
//! nodes × 24 bytes (parent u32, name_off u32, name_len u16, kind u8,
//!                   flags u8, mtime u32, size u64)
//! names bytes
//! crc32 of everything above
//! ```

use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::{Index, NONE, Node};

const MAGIC: &[u8; 8] = b"TUDIIDX\0";
/// Bump when the layout changes; older snapshots are then ignored.
pub const VERSION: u32 = 1;

/// Default snapshot path: `$XDG_CACHE_HOME/tudi/index-v{VERSION}.bin`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join(format!("tudi/index-v{VERSION}.bin")))
}

pub fn encode(ix: &Index) -> Vec<u8> {
    let mut b = Vec::with_capacity(64 + ix.nodes.len() * 24 + ix.names.len());
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&VERSION.to_le_bytes());
    b.extend_from_slice(&(ix.nodes.len() as u64).to_le_bytes());
    b.extend_from_slice(&(ix.names.len() as u64).to_le_bytes());
    b.extend_from_slice(&(ix.roots.len() as u32).to_le_bytes());
    for (p, n) in &ix.roots {
        let bytes = p.as_os_str().as_bytes();
        b.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        b.extend_from_slice(bytes);
        b.extend_from_slice(&n.to_le_bytes());
    }
    for n in &ix.nodes {
        b.extend_from_slice(&n.parent.to_le_bytes());
        b.extend_from_slice(&n.name_off.to_le_bytes());
        b.extend_from_slice(&n.name_len.to_le_bytes());
        b.push(n.kind);
        b.push(n.flags);
        b.extend_from_slice(&n.mtime.to_le_bytes());
        b.extend_from_slice(&n.size.to_le_bytes());
    }
    b.extend_from_slice(&ix.names);
    let crc = crc32fast::hash(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    b
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| bad("overflow"))?;
        let s = self.b.get(self.pos..end).ok_or_else(|| bad("truncated"))?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

pub fn decode(b: &[u8]) -> io::Result<Index> {
    if b.len() < 4 + MAGIC.len() {
        return Err(bad("too short"));
    }
    let (body, crc) = b.split_at(b.len() - 4);
    if crc32fast::hash(body).to_le_bytes() != crc {
        return Err(bad("checksum mismatch"));
    }
    let mut r = Reader { b: body, pos: 0 };
    if r.take(8)? != MAGIC {
        return Err(bad("not a tudi index"));
    }
    if r.u32()? != VERSION {
        return Err(bad("other index version"));
    }
    let n = usize::try_from(r.u64()?).map_err(|_| bad("size"))?;
    let names_len = usize::try_from(r.u64()?).map_err(|_| bad("size"))?;
    let nroots = r.u32()?;
    let mut roots = Vec::new();
    for _ in 0..nroots {
        let len = r.u32()? as usize;
        let p = PathBuf::from(std::ffi::OsString::from_vec(r.take(len)?.to_vec()));
        roots.push((p, r.u32()?));
    }
    if n.checked_mul(24).is_none_or(|x| x > body.len()) {
        return Err(bad("node count"));
    }
    let mut ix = Index::new();
    ix.nodes.reserve_exact(n);
    for _ in 0..n {
        ix.nodes.push(Node {
            parent: r.u32()?,
            name_off: r.u32()?,
            name_len: r.u16()?,
            kind: r.u8()?,
            flags: r.u8()?,
            mtime: r.u32()?,
            size: r.u64()?,
        });
    }
    ix.names = r.take(names_len)?.to_vec();
    // Validate and rebuild child lists (parent-before-child order).
    ix.first_child = vec![NONE; n];
    ix.next_sibling = vec![NONE; n];
    let mut dead = 0;
    for i in 0..n {
        let node = ix.nodes[i];
        if node.name_off as usize + usize::from(node.name_len) > ix.names.len() {
            return Err(bad("name out of range"));
        }
        if node.is_dead() {
            dead += 1;
        }
        if node.parent != NONE {
            let p = node.parent as usize;
            if p >= i {
                return Err(bad("parent after child"));
            }
            ix.next_sibling[i] = ix.first_child[p];
            ix.first_child[p] = i as u32;
        }
    }
    if roots.iter().any(|(_, r)| *r as usize >= n) {
        return Err(bad("root out of range"));
    }
    ix.roots = roots;
    ix.set_tombstones(dead);
    Ok(ix)
}

/// Write `ix` to `path` atomically.
pub fn save(ix: &Index, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("bin.tmp");
    let data = encode(ix);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> io::Result<Index> {
    decode(&std::fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entry, Kind};

    fn sample() -> Index {
        let mut ix = Index::new();
        let r = ix.add_root(Path::new("/w"), 1_800_000_000);
        let e = |n: &str, k| Entry {
            name: n.as_bytes().to_vec(),
            kind: k,
            size: 7,
            mtime: 1_800_000_000,
        };
        let (d, _) = ix.apply_listing(r, &[e("a", Kind::Dir), e("合同.xlsx", Kind::File)]);
        ix.apply_listing(d[0], &[e("x", Kind::File)]);
        ix
    }

    #[test]
    fn round_trip() {
        let ix = sample();
        let back = decode(&encode(&ix)).unwrap();
        assert_eq!(back.nodes, ix.nodes);
        assert_eq!(back.names, ix.names);
        assert_eq!(back.roots, ix.roots);
        assert!(back.find(Path::new("/w/a/x")).is_some());
        assert_eq!(back.len(), ix.len());
    }

    #[test]
    fn corruption_is_rejected() {
        let mut b = encode(&sample());
        let mid = b.len() / 2;
        b[mid] ^= 0xff;
        assert!(decode(&b).is_err());
        assert!(decode(b"short").is_err());
        let mut v = encode(&sample());
        v[8] = 99; // version
        let n = v.len();
        let crc = crc32fast::hash(&v[..n - 4]);
        v[n - 4..].copy_from_slice(&crc.to_le_bytes());
        assert!(decode(&v).is_err());
    }

    #[test]
    fn save_is_atomic_and_loads() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("c/index.bin");
        save(&sample(), &p).unwrap();
        // A leftover temp file from a killed write does not matter.
        std::fs::write(p.with_extension("bin.tmp"), b"garbage").unwrap();
        assert_eq!(load(&p).unwrap().len(), 4);
    }
}
