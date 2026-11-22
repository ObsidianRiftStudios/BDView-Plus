//! Minimal virtual filesystem: a plain directory, or a read-only UDF image / raw disc device.
//! UDF support covers UDF 1.02–2.50 (physical and metadata partitions), which is what Blu-rays use.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SEC: u64 = 2048;

fn err(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}
fn u16le(d: &[u8], o: usize) -> u16 {
    d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}
fn u32le(d: &[u8], o: usize) -> u32 {
    d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn u64le(d: &[u8], o: usize) -> u64 {
    d.get(o..o + 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
}
fn fill(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

/// UDF character data: first byte is the compression id (8 = bytes, 16 = UTF-16BE).
fn decode(b: &[u8]) -> String {
    match b.split_first() {
        Some((8, r)) => r.iter().map(|&c| c as char).collect(),
        Some((16, r)) => String::from_utf16_lossy(&r.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()),
        _ => String::new(),
    }
}

enum Data {
    Inline(Vec<u8>),
    /// (image byte offset, length); offset u64::MAX = sparse zeros
    Ext(Vec<(u64, u64)>),
}

struct Fe {
    is_dir: bool,
    size: u64,
    data: Data,
}

enum Pm {
    Phys(u64),
    /// metadata partition: index of the underlying physical map, extents as (length, image offset)
    Meta(usize, Vec<(u64, u64)>),
    Bad,
}

pub struct Udf {
    f: RefCell<File>,
    bs: u64,
    maps: RefCell<Vec<Pm>>,
}

impl Udf {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut f = self.f.borrow_mut();
        let eof = || io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of image");
        if off.is_multiple_of(SEC) && (buf.len() as u64).is_multiple_of(SEC) {
            f.seek(SeekFrom::Start(off))?;
            return if fill(&mut f, buf)? < buf.len() { Err(eof()) } else { Ok(()) };
        }
        let s = off / SEC * SEC;
        let e = (off + buf.len() as u64).div_ceil(SEC) * SEC;
        let mut t = vec![0u8; (e - s) as usize];
        f.seek(SeekFrom::Start(s))?;
        let n = fill(&mut f, &mut t)?;
        let a = (off - s) as usize;
        if a + buf.len() > n {
            return Err(eof());
        }
        buf.copy_from_slice(&t[a..a + buf.len()]);
        Ok(())
    }

    fn sector(&self, n: u64) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; SEC as usize];
        self.read_at(n * SEC, &mut v)?;
        Ok(v)
    }

    fn resolve(&self, pref: usize, lbn: u64) -> io::Result<u64> {
        match self.maps.borrow().get(pref) {
            Some(Pm::Phys(s)) => Ok((s + lbn) * self.bs),
            Some(Pm::Meta(_, ext)) => {
                let mut o = lbn * self.bs;
                for &(l, p) in ext {
                    if o < l {
                        return Ok(p + o);
                    }
                    o -= l;
                }
                Err(err("block outside metadata partition"))
            }
            _ => Err(err("unsupported partition")),
        }
    }

    /// Partition that holds file *data* for a file whose descriptor lives in `pref`.
    fn data_ref(&self, pref: usize) -> usize {
        match self.maps.borrow().get(pref) {
            Some(Pm::Meta(p, _)) => *p,
            _ => pref,
        }
    }

    fn fe(&self, pref: usize, lbn: u64) -> io::Result<Fe> {
        let mut d = vec![0u8; self.bs as usize];
        self.read_at(self.resolve(pref, lbn)?, &mut d)?;
        let (hdr, ea, adl) = match u16le(&d, 0) {
            261 => (176, 168, 172),
            266 => (216, 208, 212),
            _ => return Err(err("bad file entry")),
        };
        let is_dir = d[27] == 4;
        let flags = u16le(&d, 34);
        let size = u64le(&d, 56);
        let start = hdr + u32le(&d, ea) as usize;
        let end = (start + u32le(&d, adl) as usize).min(d.len());
        let ads = d.get(start..end).unwrap_or(&[]);
        let alloc = flags & 7;
        if alloc == 3 {
            return Ok(Fe {
                is_dir,
                size,
                data: Data::Inline(ads.to_vec()),
            });
        }
        let step = match alloc {
            0 => 8,
            1 => 16,
            _ => return Err(err("unsupported allocation type")),
        };
        let dpref = if is_dir { pref } else { self.data_ref(pref) };
        let (mut buf, mut p, mut ext) = (ads.to_vec(), 0, Vec::new());
        while p + step <= buf.len() {
            let w = u32le(&buf, p);
            let (len, ty, lbn) = ((w & 0x3FFF_FFFF) as u64, w >> 30, u32le(&buf, p + 4) as u64);
            let ap = if alloc == 1 {
                let r = u16le(&buf, p + 8) as usize;
                if is_dir {
                    r
                } else {
                    self.data_ref(r)
                }
            } else {
                dpref
            };
            p += step;
            if len == 0 {
                break;
            }
            match ty {
                0 => ext.push((self.resolve(ap, lbn)?, len)),
                3 => {
                    let mut a = vec![0u8; self.bs as usize];
                    self.read_at(self.resolve(ap, lbn)?, &mut a)?;
                    if u16le(&a, 0) != 258 {
                        return Err(err("bad allocation extent descriptor"));
                    }
                    let l = (u32le(&a, 20) as usize).min(a.len() - 24);
                    buf = a[24..24 + l].to_vec();
                    p = 0;
                }
                _ => ext.push((u64::MAX, len)),
            }
        }
        Ok(Fe {
            is_dir,
            size,
            data: Data::Ext(ext),
        })
    }

    fn node(self: &Rc<Self>, name: String, pref: usize, lbn: u64) -> io::Result<Node> {
        let fe = self.fe(pref, lbn)?;
        Ok(Node {
            name,
            is_dir: fe.is_dir,
            size: fe.size,
            src: Src::Udf(self.clone(), Rc::new(fe.data)),
        })
    }

    fn dir(self: &Rc<Self>, data: &Rc<Data>, size: u64) -> io::Result<Vec<Node>> {
        let mut b = Vec::new();
        UdfReader {
            u: self.clone(),
            data: data.clone(),
            size,
            pos: 0,
        }
        .read_to_end(&mut b)?;
        let (mut p, mut out) = (0, Vec::new());
        while p + 38 <= b.len() && u16le(&b, p) == 257 {
            let chars = b[p + 18];
            let lfi = b[p + 19] as usize;
            let (lbn, pref) = (u32le(&b, p + 24) as u64, u16le(&b, p + 28) as usize);
            let liu = u16le(&b, p + 36) as usize;
            let n0 = p + 38 + liu;
            if chars & 0x0C == 0 {
                let name = decode(b.get(n0..n0 + lfi).unwrap_or(&[]));
                if let Ok(n) = self.node(name, pref, lbn) {
                    out.push(n);
                }
            }
            p = (n0 + lfi + 3) & !3;
        }
        Ok(out)
    }
}

struct UdfReader {
    u: Rc<Udf>,
    data: Rc<Data>,
    size: u64,
    pos: u64,
}

impl Read for UdfReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.size - self.pos) as usize;
        match &*self.data {
            Data::Inline(v) => {
                let p = self.pos as usize;
                let n = want.min(v.len().saturating_sub(p));
                buf[..n].copy_from_slice(&v[p..p + n]);
                self.pos = if n == 0 { self.size } else { self.pos + n as u64 };
                Ok(n)
            }
            Data::Ext(e) => {
                let mut o = self.pos;
                for &(off, len) in e {
                    if o >= len {
                        o -= len;
                        continue;
                    }
                    let mut n = want.min((len - o) as usize);
                    // keep reads sector-aligned except for the final piece of an extent / file
                    if n as u64 > SEC && o + (n as u64) < len && self.pos + (n as u64) < self.size {
                        n -= n % SEC as usize;
                    }
                    if off == u64::MAX {
                        buf[..n].fill(0);
                    } else {
                        self.u.read_at(off + o, &mut buf[..n])?;
                    }
                    self.pos += n as u64;
                    return Ok(n);
                }
                self.pos = self.size;
                Ok(0)
            }
        }
    }
}

#[derive(Clone)]
enum Src {
    Fs(PathBuf),
    Udf(Rc<Udf>, Rc<Data>),
    #[cfg(feature = "bluray")]
    Bd(std::sync::Arc<crate::bluray::Disc>, String),
}

/// A file or directory on a directory tree or inside a UDF image.
#[derive(Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    src: Src,
}

impl Node {
    pub fn fs(p: &Path) -> Node {
        let m = fs::metadata(p).ok();
        Node {
            name: p.file_name().map(|n| n.to_string_lossy().into()).unwrap_or_default(),
            is_dir: m.as_ref().is_some_and(|m| m.is_dir()),
            size: m.map_or(0, |m| m.len()),
            src: Src::Fs(p.into()),
        }
    }
    pub fn children(&self) -> Vec<Node> {
        match &self.src {
            Src::Fs(p) => fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| Node::fs(&e.path()))
                .collect(),
            Src::Udf(u, d) => u.dir(d, self.size).unwrap_or_default(),
            #[cfg(feature = "bluray")]
            Src::Bd(..) => Vec::new(),
        }
    }
    /// The same file, read through libbluray so that encrypted data comes out decrypted.
    #[cfg(feature = "bluray")]
    pub fn decrypted(&self, d: &std::sync::Arc<crate::bluray::Disc>) -> Node {
        Node {
            src: Src::Bd(d.clone(), self.name.clone()),
            ..self.clone()
        }
    }
    pub fn find(&self, name: &str) -> Option<Node> {
        self.children().into_iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }
    pub fn open(&self) -> io::Result<Box<dyn Read>> {
        Ok(match &self.src {
            Src::Fs(p) => Box::new(File::open(p)?),
            Src::Udf(u, d) => Box::new(UdfReader {
                u: u.clone(),
                data: d.clone(),
                size: self.size,
                pos: 0,
            }),
            #[cfg(feature = "bluray")]
            Src::Bd(d, name) => Box::new(d.open_stream(name)?),
        })
    }
    pub fn read(&self) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        self.open()?.read_to_end(&mut v)?;
        Ok(v)
    }
}

/// Open a UDF image file or raw optical device. Returns the root directory and volume label.
pub fn open_udf(path: &Path) -> io::Result<(Node, String)> {
    let mut f = File::open(path)?;
    let len = f.seek(SeekFrom::End(0))?;
    let mut u = Udf {
        f: RefCell::new(f),
        bs: SEC,
        maps: RefCell::new(Vec::new()),
    };
    let last = (len / SEC).saturating_sub(1);
    let avdp = [256, last, last.saturating_sub(256)]
        .iter()
        .find_map(|&s| u.sector(s).ok().filter(|d| u16le(d, 0) == 2))
        .ok_or_else(|| err("not a UDF image (no anchor volume descriptor)"))?;

    let (mut label, mut parts, mut lvd) = (String::new(), Vec::<(u16, u64)>::new(), None::<Vec<u8>>);
    'vds: for at in [16usize, 24] {
        let (vlen, vloc) = (u32le(&avdp, at) as u64, u32le(&avdp, at + 4) as u64);
        for i in 0..(vlen / SEC).min(64) {
            let Ok(d) = u.sector(vloc + i) else { break };
            match u16le(&d, 0) {
                1 if label.is_empty() => {
                    let l = (d[24 + 31] as usize).clamp(1, 31);
                    label = decode(&d[24..24 + l]).trim().to_string();
                }
                5 => parts.push((u16le(&d, 22), u32le(&d, 188) as u64)),
                6 if lvd.is_none() => lvd = Some(d),
                8 => break,
                _ => {}
            }
        }
        if lvd.is_some() && !parts.is_empty() {
            break 'vds;
        }
    }
    let lvd = lvd.ok_or_else(|| err("UDF logical volume descriptor not found"))?;
    let bs = u32le(&lvd, 212) as u64;
    u.bs = if bs.is_power_of_two() && (512..=65536).contains(&bs) {
        bs
    } else {
        SEC
    };

    let start_of = |n: u16| parts.iter().find(|p| p.0 == n).map(|p| p.1);
    let (mut maps, mut phys_nums, mut pending) = (Vec::new(), Vec::new(), Vec::new());
    let mut off = 440;
    for _ in 0..u32le(&lvd, 268) {
        let (t, l) = (lvd.get(off).copied().unwrap_or(0), lvd.get(off + 1).copied().unwrap_or(0) as usize);
        match t {
            1 => {
                let n = u16le(&lvd, off + 4);
                maps.push(start_of(n).map_or(Pm::Bad, Pm::Phys));
                phys_nums.push(Some(n));
            }
            2 if lvd.get(off + 5..off + 36).is_some_and(|id| id.windows(8).any(|w| w == b"Metadata")) => {
                let n = u16le(&lvd, off + 38);
                pending.push((maps.len(), n, u32le(&lvd, off + 40) as u64));
                maps.push(Pm::Bad);
                phys_nums.push(None);
            }
            _ => {
                maps.push(Pm::Bad);
                phys_nums.push(None);
            }
        }
        off += l.max(2);
    }
    *u.maps.borrow_mut() = maps;
    let u = Rc::new(u);
    for (idx, num, loc) in pending {
        let phys = phys_nums
            .iter()
            .position(|&p| p == Some(num))
            .ok_or_else(|| err("metadata partition has no physical partition"))?;
        let fe = u.fe(phys, loc)?;
        let Data::Ext(e) = fe.data else {
            return Err(err("unsupported metadata file layout"));
        };
        u.maps.borrow_mut()[idx] = Pm::Meta(phys, e.into_iter().map(|(o, l)| (l, o)).collect());
    }

    let (fpart, flbn) = (u16le(&lvd, 248 + 8) as usize, u32le(&lvd, 248 + 4) as u64);
    let mut fsd = vec![0u8; u.bs as usize];
    u.read_at(u.resolve(fpart, flbn)?, &mut fsd)?;
    if u16le(&fsd, 0) != 256 {
        return Err(err("UDF file set descriptor not found"));
    }
    let root = u.node(String::new(), u16le(&fsd, 408) as usize, u32le(&fsd, 404) as u64)?;
    if label.is_empty() {
        label = path.file_stem().map(|s| s.to_string_lossy().into()).unwrap_or_default();
    }
    Ok((root, label))
}
