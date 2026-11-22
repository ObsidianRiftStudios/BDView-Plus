//! Saved bitrate scans. After a bitrate scan the measurements are written to one file per disc in a
//! single folder (`<config>/tuxinfo/scans`); opening the same disc again loads them instead of
//! reading every stream file. The disc is recognised by a fingerprint of its playlists and stream
//! file sizes, never by its path, so the same disc from a drive, a folder or an ISO matches.

use crate::disc::{Diag, Disc, StreamFile};
use crate::stream::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const MAGIC: &[u8; 4] = b"TXSC";
/// Bump when the scan results change meaning (new fields, different algorithm) so old files are ignored.
const VERSION: u32 = 1;

pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("tuxinfo").join("scans"))
}

// ---- tiny binary codec -------------------------------------------------------------------------

struct Wr(Vec<u8>);

impl Wr {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, v: &str) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v.as_bytes());
    }
    fn opt_str(&mut self, v: &Option<String>) {
        match v {
            Some(s) => {
                self.bool(true);
                self.str(s);
            }
            None => self.bool(false),
        }
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn opt_str(&mut self) -> Option<Option<String>> {
        Some(if self.bool()? { Some(self.str()?) } else { None })
    }
    /// A count that cannot exceed what is left of the file (each item takes at least `min` bytes).
    fn count(&mut self, min: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.checked_mul(min)? <= self.b.len() - self.p).then_some(n)
    }
}

fn mode_code(m: AudioMode) -> u8 {
    match m {
        AudioMode::Unknown => 0,
        AudioMode::DualMono => 1,
        AudioMode::Stereo => 2,
        AudioMode::Surround => 3,
        AudioMode::Extended => 4,
        AudioMode::JointStereo => 5,
        AudioMode::Mono => 6,
    }
}

fn mode_from(c: u8) -> AudioMode {
    match c {
        1 => AudioMode::DualMono,
        2 => AudioMode::Stereo,
        3 => AudioMode::Surround,
        4 => AudioMode::Extended,
        5 => AudioMode::JointStereo,
        6 => AudioMode::Mono,
        _ => AudioMode::Unknown,
    }
}

/// Every field of a stream except the HEVC parser state, which only the scan itself needs.
/// Destructured in full so that adding a field to `Stream` stops the build until it is saved here.
fn put_stream(w: &mut Wr, s: &Stream) {
    let Stream {
        pid, stype, lang, lang_name, bitrate, active_bitrate, is_vbr, initialized, hidden, payload_bytes, packet_count, packet_seconds, height, interlaced, fr_num, fr_den,
        aspect, profile, ext_info, hevc: _, layout, sample_rate, channels, bit_depth, lfe, dial_norm, has_ext, mode, core, codec_text, width, captions, forced_captions, last_frame,
        caption_ids,
    } = s;
    w.u16(*pid);
    w.u8(*stype);
    w.str(lang);
    w.str(lang_name);
    w.i64(*bitrate);
    w.i64(*active_bitrate);
    w.bool(*is_vbr);
    w.bool(*initialized);
    w.bool(*hidden);
    w.u64(*payload_bytes);
    w.u64(*packet_count);
    w.f64(*packet_seconds);
    w.i32(*height);
    w.bool(*interlaced);
    w.i32(*fr_num);
    w.i32(*fr_den);
    w.u8(*aspect);
    w.opt_str(profile);
    w.u32(ext_info.len() as u32);
    for e in ext_info {
        w.str(e);
    }
    w.u8(*layout);
    w.i32(*sample_rate);
    w.i32(*channels);
    w.i32(*bit_depth);
    w.i32(*lfe);
    w.i32(*dial_norm);
    w.bool(*has_ext);
    w.u8(mode_code(*mode));
    match core {
        Some(c) => {
            w.bool(true);
            put_stream(w, c);
        }
        None => w.bool(false),
    }
    w.str(codec_text);
    w.i32(*width);
    w.i32(*captions);
    w.i32(*forced_captions);
    w.bool(last_frame.forced);
    w.bool(last_frame.finished);
    let mut ids: Vec<u32> = caption_ids.iter().copied().collect();
    ids.sort_unstable();
    w.u32(ids.len() as u32);
    for i in ids {
        w.u32(i);
    }
}

fn get_stream(r: &mut Rd, depth: u8) -> Option<Stream> {
    if depth > 2 {
        return None;
    }
    let mut s = Stream { pid: r.u16()?, stype: r.u8()?, ..Default::default() };
    s.lang = r.str()?;
    s.lang_name = r.str()?;
    s.bitrate = r.i64()?;
    s.active_bitrate = r.i64()?;
    s.is_vbr = r.bool()?;
    s.initialized = r.bool()?;
    s.hidden = r.bool()?;
    s.payload_bytes = r.u64()?;
    s.packet_count = r.u64()?;
    s.packet_seconds = r.f64()?;
    s.height = r.i32()?;
    s.interlaced = r.bool()?;
    s.fr_num = r.i32()?;
    s.fr_den = r.i32()?;
    s.aspect = r.u8()?;
    s.profile = r.opt_str()?;
    let n = r.count(4)?;
    for _ in 0..n {
        s.ext_info.push(r.str()?);
    }
    s.layout = r.u8()?;
    s.sample_rate = r.i32()?;
    s.channels = r.i32()?;
    s.bit_depth = r.i32()?;
    s.lfe = r.i32()?;
    s.dial_norm = r.i32()?;
    s.has_ext = r.bool()?;
    s.mode = mode_from(r.u8()?);
    s.core = if r.bool()? { Some(Box::new(get_stream(r, depth + 1)?)) } else { None };
    s.codec_text = r.str()?;
    s.width = r.i32()?;
    s.captions = r.i32()?;
    s.forced_captions = r.i32()?;
    s.last_frame.forced = r.bool()?;
    s.last_frame.finished = r.bool()?;
    let n = r.count(4)?;
    for _ in 0..n {
        s.caption_ids.insert(r.u32()?);
    }
    Some(s)
}

fn put_set(w: &mut Wr, set: &StreamSet) {
    w.u32(set.list.len() as u32);
    for s in &set.list {
        put_stream(w, s);
    }
}

fn get_set(r: &mut Rd) -> Option<StreamSet> {
    let n = r.count(50)?;
    let mut set = StreamSet::default();
    for _ in 0..n {
        set.insert(get_stream(r, 0)?);
    }
    Some(set)
}

/// Frame type labels are `&'static str` in the scan; give loaded ones the same shape.
fn intern(s: &str) -> &'static str {
    static T: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut t = T.lock().unwrap();
    if let Some(x) = t.iter().find(|x| **x == s) {
        return x;
    }
    let x: &'static str = Box::leak(s.to_string().into_boxed_str());
    t.push(x);
    x
}

// ---- records ----------------------------------------------------------------------------------

pub struct FileRec {
    name: String,
    length: f64,
    streams: StreamSet,
    diag: Vec<(u16, Vec<Diag>)>,
}

pub struct PlRec {
    name: String,
    hidden: bool,
    loops: bool,
    streams: StreamSet,
    /// (payload bytes, packet count, packet seconds) of each clip.
    clips: Vec<(u64, u64, f64)>,
}

pub struct Cache {
    extended: bool,
    when: u64,
    warnings: Vec<String>,
    files: Vec<FileRec>,
    pls: Vec<PlRec>,
}

fn encode(fp: u64, label: &str, c: &Cache) -> Vec<u8> {
    let mut w = Wr(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.u64(fp);
    w.bool(c.extended);
    w.u64(c.when);
    w.str(label);
    w.u32(c.warnings.len() as u32);
    for x in &c.warnings {
        w.str(x);
    }
    w.u32(c.files.len() as u32);
    for f in &c.files {
        w.str(&f.name);
        w.f64(f.length);
        put_set(&mut w, &f.streams);
        w.u32(f.diag.len() as u32);
        for (pid, v) in &f.diag {
            w.u16(*pid);
            // frame type labels once per stream, then 1-byte references
            let mut tags: Vec<&str> = Vec::new();
            for g in v {
                if !tags.contains(&g.tag) {
                    tags.push(g.tag);
                }
            }
            w.u32(tags.len() as u32);
            for t in &tags {
                w.str(t);
            }
            w.u32(v.len() as u32);
            for g in v {
                w.f64(g.marker);
                w.f64(g.interval);
                w.u32(g.bytes.min(u32::MAX as u64) as u32);
                w.u8(tags.iter().position(|t| *t == g.tag).unwrap_or(0) as u8);
            }
        }
    }
    w.u32(c.pls.len() as u32);
    for p in &c.pls {
        w.str(&p.name);
        w.bool(p.hidden);
        w.bool(p.loops);
        put_set(&mut w, &p.streams);
        w.u32(p.clips.len() as u32);
        for (a, b, s) in &p.clips {
            w.u64(*a);
            w.u64(*b);
            w.f64(*s);
        }
    }
    w.0
}

/// Parse a file; None if it is damaged, from another version, or for another disc.
fn decode(data: &[u8], fp: u64) -> Option<Cache> {
    let mut r = Rd { b: data, p: 0 };
    if r.take(4)? != MAGIC || r.u32()? != VERSION || r.u64()? != fp {
        return None;
    }
    let extended = r.bool()?;
    let when = r.u64()?;
    let _label = r.str()?;
    let n = r.count(4)?;
    let mut warnings = Vec::new();
    for _ in 0..n {
        warnings.push(r.str()?);
    }
    let n = r.count(16)?;
    let mut files = Vec::new();
    for _ in 0..n {
        let name = r.str()?;
        let length = r.f64()?;
        let streams = get_set(&mut r)?;
        let nd = r.count(10)?;
        let mut diag = Vec::new();
        for _ in 0..nd {
            let pid = r.u16()?;
            let nt = r.count(4)?;
            let mut tags: Vec<&'static str> = Vec::new();
            for _ in 0..nt {
                tags.push(intern(&r.str()?));
            }
            let n = r.count(21)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                let (marker, interval, bytes, t) = (r.f64()?, r.f64()?, r.u32()?, r.u8()?);
                v.push(Diag { marker, interval, bytes: bytes as u64, tag: tags.get(t as usize).copied().unwrap_or("") });
            }
            diag.push((pid, v));
        }
        files.push(FileRec { name, length, streams, diag });
    }
    let n = r.count(16)?;
    let mut pls = Vec::new();
    for _ in 0..n {
        let name = r.str()?;
        let hidden = r.bool()?;
        let loops = r.bool()?;
        let streams = get_set(&mut r)?;
        let nc = r.count(24)?;
        let mut clips = Vec::with_capacity(nc);
        for _ in 0..nc {
            clips.push((r.u64()?, r.u64()?, r.f64()?));
        }
        pls.push(PlRec { name, hidden, loops, streams, clips });
    }
    Some(Cache { extended, when, warnings, files, pls })
}

// ---- disc <-> cache ---------------------------------------------------------------------------

/// Fingerprint of the disc's layout: stream file names and sizes, playlist names and their clips.
fn fingerprint(d: &Disc) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h ^= x as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    };
    let mut files: Vec<(&String, u64)> = d.files.iter().map(|(k, f)| (k, f.size)).collect();
    files.sort();
    for (k, s) in files {
        eat(k.as_bytes());
        eat(&s.to_le_bytes());
    }
    let mut pls: Vec<_> = d.playlists.iter().filter(|p| !p.custom).collect();
    pls.sort_by(|a, b| a.name.cmp(&b.name));
    for p in pls {
        eat(p.name.as_bytes());
        for c in &p.clips {
            eat(c.name.as_bytes());
            eat(&c.time_in.to_bits().to_le_bytes());
            eat(&c.time_out.to_bits().to_le_bytes());
            eat(&[c.angle as u8, c.has_file as u8]);
        }
    }
    h
}

fn file_name(d: &Disc, fp: u64) -> String {
    let label: String = d.label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(40).collect();
    format!("{}-{:016x}.scan", if label.is_empty() { "disc" } else { &label }, fp)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// "2026-11-21" (UTC).
pub fn date(when: u64) -> String {
    let z = (when / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!("{year:04}-{month:02}-{day:02}")
}

/// Records for the named playlists as the disc holds them now.
fn collect(d: &Disc, names: &[String]) -> (Vec<FileRec>, Vec<PlRec>) {
    let mut files: Vec<FileRec> = Vec::new();
    let mut pls = Vec::new();
    for p in d.playlists.iter().filter(|p| !p.custom && names.contains(&p.name)) {
        if !p.clips.iter().any(|c| c.packet_count > 0) {
            continue;
        }
        for c in p.clips.iter().filter(|c| c.has_file) {
            if files.iter().any(|f| f.name == c.name) {
                continue;
            }
            let Some(f) = d.files.get(&c.name) else { continue };
            files.push(file_rec(f));
        }
        pls.push(PlRec {
            name: p.name.clone(),
            hidden: p.has_hidden_tracks,
            loops: p.has_loops,
            streams: p.streams.clone(),
            clips: p.clips.iter().map(|c| (c.payload_bytes, c.packet_count, c.packet_seconds)).collect(),
        });
    }
    (files, pls)
}

fn file_rec(f: &StreamFile) -> FileRec {
    let mut diag: Vec<(u16, Vec<Diag>)> = f.diag.iter().map(|(p, v)| (*p, v.iter().map(|g| Diag { marker: g.marker, interval: g.interval, bytes: g.bytes, tag: g.tag }).collect())).collect();
    diag.sort_by_key(|x| x.0);
    FileRec { name: f.name.clone(), length: f.length, streams: f.streams.clone(), diag }
}

/// Write the scan of `names` (the playlists just scanned) next to anything saved earlier for the
/// same disc. Returns the file written.
pub fn save(d: &Disc, extended: bool, names: &[String], warnings: &[String]) -> std::io::Result<PathBuf> {
    let dir = dir().ok_or_else(|| std::io::Error::other("no config folder"))?;
    std::fs::create_dir_all(&dir)?;
    let fp = fingerprint(d);
    let path = dir.join(file_name(d, fp));
    let (files, pls) = collect(d, names);
    let mut c = Cache { extended, when: now(), warnings: warnings.to_vec(), files, pls };
    // keep playlists saved by earlier scans of this disc (same settings) that this scan did not redo
    if let Some(old) = std::fs::read(&path).ok().and_then(|b| decode(&b, fp)).filter(|o| o.extended == extended) {
        for p in old.pls {
            if !c.pls.iter().any(|x| x.name == p.name) {
                c.pls.push(p);
            }
        }
        for f in old.files {
            if !c.files.iter().any(|x| x.name == f.name) {
                c.files.push(f);
            }
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, encode(fp, &d.label, &c))?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

pub struct Loaded {
    pub when: u64,
    pub playlists: usize,
    pub warnings: Vec<String>,
}

/// Is there a saved scan file for this disc (whether or not it can be used)?
pub fn exists(d: &Disc) -> bool {
    dir().is_some_and(|p| p.join(file_name(d, fingerprint(d))).is_file())
}

/// Fill the disc's measurements from its saved scan, if there is a usable one.
pub fn load(d: &mut Disc, extended: bool) -> Option<Loaded> {
    let fp = fingerprint(d);
    let path = dir()?.join(file_name(d, fp));
    let c = decode(&std::fs::read(path).ok()?, fp).filter(|c| c.extended == extended)?;
    let mut applied = 0;
    for rec in c.files {
        if let Some(f) = d.files.get_mut(&rec.name) {
            f.streams = rec.streams;
            f.length = rec.length;
            f.diag = rec.diag.into_iter().collect::<HashMap<_, _>>();
        }
    }
    for rec in c.pls {
        let Some(p) = d.playlists.iter_mut().find(|p| !p.custom && p.name == rec.name) else { continue };
        if p.clips.len() != rec.clips.len() {
            continue;
        }
        p.streams = rec.streams;
        p.has_hidden_tracks = rec.hidden;
        p.has_loops = rec.loops;
        for (cl, (b, n, s)) in p.clips.iter_mut().zip(rec.clips) {
            cl.payload_bytes = b;
            cl.packet_count = n;
            cl.packet_seconds = s;
        }
        applied += 1;
    }
    if applied == 0 {
        return None;
    }
    d.refresh_flags();
    Some(Loaded { when: c.when, playlists: applied, warnings: c.warnings })
}

/// Delete every saved scan; returns how many files were removed.
pub fn clear() -> usize {
    let Some(dir) = dir() else { return 0 };
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "scan") && std::fs::remove_file(e.path()).is_ok()).count()
}

/// (files, total bytes) of the saved scans.
pub fn usage() -> (usize, u64) {
    let Some(dir) = dir() else { return (0, 0) };
    let Ok(rd) = std::fs::read_dir(dir) else { return (0, 0) };
    rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "scan")).fold((0, 0), |(n, b), e| (n + 1, b + e.metadata().map_or(0, |m| m.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report;
    use crate::scan::{self, Mode};

    /// A disc reopened from its saved scan must report exactly what the fresh scan reported.
    #[test]
    fn saved_scan_reports_the_same() {
        let tmp = std::env::temp_dir().join(format!("tuxinfo-scancache-test-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
        for path in std::env::var("CACHE_DISCS").unwrap_or("/tmp/pv:/tmp/hvdisc".into()).split(':') {
            let Ok(mut a) = Disc::open(std::path::Path::new(path)) else { continue };
            a.finish();
            let sel: Vec<usize> = (0..a.playlists.len()).collect();
            let warnings = scan::scan(&mut a, &sel, Mode::Both, &mut |_| true);
            let names: Vec<String> = a.playlists.iter().map(|p| p.name.clone()).collect();
            let opts = || report::Options { diag: true, summary: true, warnings: warnings.clone() };
            let want = report::generate(&a, &sel, &opts());
            save(&a, false, &names, &warnings).unwrap();
            let mut b = Disc::open(std::path::Path::new(path)).unwrap();
            b.finish();
            assert!(load(&mut b, true).is_none(), "different settings must not load");
            let l = load(&mut b, false).expect("saved scan loads");
            assert_eq!(l.playlists, a.playlists.len());
            let got = report::generate(&b, &sel, &opts());
            assert_eq!(want, got, "report differs for {path}");
            assert_eq!(a.is_50hz, b.is_50hz);
            // damaged file: never panics, just ignored
            let f = dir().unwrap().join(file_name(&b, fingerprint(&b)));
            let mut bytes = std::fs::read(&f).unwrap();
            bytes.truncate(bytes.len() / 2);
            std::fs::write(&f, bytes).unwrap();
            let mut c = Disc::open(std::path::Path::new(path)).unwrap();
            c.finish();
            assert!(load(&mut c, false).is_none());
            println!("{path}: ok ({} bytes of report)", want.len());
        }
        assert!(clear() > 0);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_795_300_000), "2026-11-21");
    }
}
