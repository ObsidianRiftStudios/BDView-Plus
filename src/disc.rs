//! Disc layout: BDMV directory, CLPI / MPLS parsing, playlist stream merging
//! (ports of BDROM.cs, TSStreamClipFile.cs and TSPlaylistFile.cs).

use crate::stream::*;
use crate::vfs::{self, Node};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Diag {
    pub marker: f64,
    pub interval: f64,
    pub bytes: u64,
    pub tag: &'static str,
}

#[derive(Default)]
pub struct StreamFile {
    pub name: String,
    pub node: Option<Node>,
    pub size: u64,
    pub streams: StreamSet,
    pub length: f64,
    pub diag: HashMap<u16, Vec<Diag>>,
}

#[derive(Clone, Default)]
pub struct Clip {
    pub name: String,
    pub clpi: String,
    pub has_file: bool,
    pub angle: usize,
    pub time_in: f64,
    pub time_out: f64,
    pub rel_in: f64,
    pub rel_out: f64,
    pub length: f64,
    pub rel_length: f64,
    pub file_size: u64,
    pub payload_bytes: u64,
    pub packet_count: u64,
    pub packet_seconds: f64,
    pub chapters: Vec<f64>,
}

impl Clip {
    pub fn packet_size(&self) -> u64 {
        self.packet_count * 192
    }
    pub fn packet_bitrate(&self) -> u64 {
        if self.packet_seconds > 0.0 {
            rnd(self.packet_size() as f64 * 8.0 / self.packet_seconds) as u64
        } else {
            0
        }
    }
}

#[derive(Default)]
pub struct Playlist {
    pub name: String,
    pub clips: Vec<Clip>,
    pub streams: StreamSet,
    pub chapters: Vec<f64>,
    pub angle_count: usize,
    pub has_hidden_tracks: bool,
    pub has_loops: bool,
    pub initialized: bool,
    /// Built by the user from clips (not read from the disc, and not saved with the scan).
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub custom: bool,
    listed: HashSet<u16>,
}

impl Playlist {
    pub fn total_length(&self) -> f64 {
        self.clips.iter().filter(|c| c.angle == 0).map(|c| c.length).sum()
    }
    pub fn file_size(&self) -> u64 {
        self.clips.iter().map(|c| c.file_size).sum()
    }
    pub fn total_size(&self) -> u64 {
        self.clips.iter().filter(|c| c.angle == 0).map(Clip::packet_size).sum()
    }
    pub fn total_bitrate(&self) -> u64 {
        let l = self.total_length();
        if l > 0.0 {
            rnd(self.total_size() as f64 * 8.0 / l) as u64
        } else {
            0
        }
    }
    /// Streams in report order: video, audio, graphics, text.
    pub fn sorted(&self) -> Vec<&Stream> {
        let f = |p: fn(&Stream) -> bool| self.streams.list.iter().filter(move |s| p(s));
        f(Stream::is_video)
            .chain(f(Stream::is_audio))
            .chain(f(Stream::is_graphics))
            .chain(f(Stream::is_text))
            .collect()
    }
    pub fn detect_loops(&self) -> bool {
        let mut seen: HashMap<&str, Vec<f64>> = HashMap::new();
        for c in self.clips.iter().filter(|c| c.angle == 0) {
            let v = seen.entry(&c.name).or_default();
            if v.contains(&c.time_in) {
                return true;
            }
            v.push(c.time_in);
        }
        false
    }
    pub fn clear_bitrates(&mut self, files: &mut HashMap<String, StreamFile>) {
        for c in &mut self.clips {
            c.payload_bytes = 0;
            c.packet_count = 0;
            c.packet_seconds = 0.0;
            if let Some(f) = files.get_mut(&c.name) {
                for s in &mut f.streams.list {
                    s.payload_bytes = 0;
                    s.packet_count = 0;
                    s.packet_seconds = 0.0;
                }
                f.diag.clear();
            }
        }
        for s in &mut self.streams.list {
            s.payload_bytes = 0;
            s.packet_count = 0;
            s.packet_seconds = 0.0;
        }
    }

    /// Port of Initialize/LoadStreamClips: choose the reference clip, clone its CLPI streams
    /// and merge what the TS scan learned about them.
    pub fn initialize(&mut self, clpis: &HashMap<String, StreamSet>, files: &HashMap<String, StreamFile>) {
        self.streams.clear();
        let n = |c: &Clip| clpis.get(&c.clpi).map_or(0, |s| s.list.len());
        let mut r = 0;
        for (i, c) in self.clips.iter().enumerate() {
            if !self.clips[r].has_file && c.has_file {
                r = i;
            }
            if (n(c) > n(&self.clips[r]) && c.rel_length > 0.01) || (c.length > self.clips[r].length && c.has_file) {
                r = i;
            }
        }
        if let Some(rc) = self.clips.get(r) {
            if let Some(cs) = clpis.get(&rc.clpi) {
                for s in &cs.list {
                    let mut s = s.clone();
                    if !self.listed.contains(&s.pid) {
                        s.hidden = true;
                        self.has_hidden_tracks = true;
                    }
                    self.streams.insert(s);
                }
            }
            if let Some(f) = files.get(&rc.name) {
                for cs in &f.streams.list {
                    let Some(s) = self.streams.get_mut(cs.pid) else { continue };
                    if s.stype != cs.stype {
                        continue;
                    }
                    s.bitrate = s.bitrate.max(cs.bitrate);
                    s.is_vbr = cs.is_vbr;
                    if s.is_video() && cs.is_video() {
                        s.profile = cs.profile.clone();
                        s.ext_info = cs.ext_info.clone();
                    } else if s.is_audio() && cs.is_audio() {
                        s.channels = s.channels.max(cs.channels);
                        s.lfe = s.lfe.max(cs.lfe);
                        s.sample_rate = s.sample_rate.max(cs.sample_rate);
                        s.bit_depth = s.bit_depth.max(cs.bit_depth);
                        s.dial_norm = s.dial_norm.min(cs.dial_norm);
                        if cs.mode != AudioMode::Unknown {
                            s.mode = cs.mode;
                        }
                        s.has_ext = cs.has_ext;
                        if s.core.is_none() {
                            s.core = cs.core.clone();
                        }
                        s.codec_text = cs.codec_text.clone();
                    } else if s.is_graphics() && cs.is_graphics() {
                        s.captions = cs.captions;
                        s.forced_captions = cs.forced_captions;
                        s.width = cs.width;
                        s.height = cs.height;
                        s.caption_ids = cs.caption_ids.clone();
                    }
                }
            }
        }
        // The Dolby Vision layer carries no HDR SEI of its own; show it with the base layer's colour and mastering text
        let base = self
            .streams
            .list
            .iter()
            .find(|s| s.stype == HEVC_VIDEO && s.pid < 4117 && !s.ext_info.is_empty())
            .map(|s| s.ext_info.clone());
        if let Some(base) = base {
            for s in self
                .streams
                .list
                .iter_mut()
                .filter(|s| s.stype == HEVC_VIDEO && s.pid >= 4117 && !s.ext_info.is_empty())
            {
                let mut e: Vec<String> = base
                    .iter()
                    .filter(|x| !x.starts_with("HDR10") && x.as_str() != "HLG")
                    .cloned()
                    .collect();
                if let Some(i) = e.iter().position(|x| x.ends_with(" bits")) {
                    e.insert(i + 1, "Dolby Vision".into());
                }
                s.ext_info = e;
            }
        }
        self.has_loops = self.detect_loops();
        self.initialized = true;
    }
}

pub struct Disc {
    pub source: String,
    pub label: String,
    pub title: Option<String>,
    pub size: u64,
    pub is_uhd: bool,
    pub is_bdplus: bool,
    pub is_bdjava: bool,
    pub is_dbox: bool,
    pub is_psp: bool,
    pub is_3d: bool,
    pub is_50hz: bool,
    pub clpis: HashMap<String, StreamSet>,
    /// Entry points (time in 45 kHz ticks, byte offset) of each clip info file, for starting playback mid-file.
    pub eps: HashMap<String, Vec<(u64, u64)>>,
    pub files: HashMap<String, StreamFile>,
    pub playlists: Vec<Playlist>,
    pub errors: Vec<String>,
    /// The open libbluray handle (shared with the preview player).
    #[cfg(feature = "bluray")]
    pub bd: Option<std::sync::Arc<crate::bluray::Disc>>,
}

pub fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

fn list_ext(dir: &Node, ext: &str) -> Vec<Node> {
    let dot = format!(".{ext}");
    let mut v: Vec<Node> = dir
        .children()
        .into_iter()
        .filter(|n| !n.is_dir && n.name.to_lowercase().ends_with(&dot))
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// BDVIEW_PLUS_LIST=<file>: write "size path" for every file BDView+ sees (same layout as
/// `find DIR -type f -printf '%s %P\n' | sort -k2`), to compare against another tool's listing.
fn list_files(d: &Node, prefix: &str, out: &mut Vec<String>) {
    for c in d.children() {
        let p = if prefix.is_empty() {
            c.name.clone()
        } else {
            format!("{prefix}/{}", c.name)
        };
        if c.is_dir {
            list_files(&c, &p, out);
        } else {
            out.push(format!("{} {p}", c.size));
        }
    }
}

fn dir_size(d: &Node) -> u64 {
    d.children()
        .iter()
        .map(|c| {
            if !c.is_dir {
                c.size
            } else if c.name.eq_ignore_ascii_case("SSIF") {
                0
            } else {
                dir_size(c)
            }
        })
        .sum()
}

fn i16be(d: &[u8], p: usize) -> usize {
    ((d[p] as usize) << 8) | d[p + 1] as usize
}
fn i32be(d: &[u8], p: usize) -> i64 {
    ((d[p] as i64) << 24) | ((d[p + 1] as i64) << 16) | ((d[p + 2] as i64) << 8) | d[p + 3] as i64
}
fn ascii(d: &[u8], p: usize, n: usize) -> String {
    d[p..p + n].iter().map(|&b| b as char).collect()
}

type StepFn = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
static ON_STEP: std::sync::Mutex<Option<StepFn>> = std::sync::Mutex::new(None);
/// Receive a short description of each step of `Disc::open` (None to stop).
pub fn on_step(f: Option<StepFn>) {
    *ON_STEP.lock().unwrap() = f;
}
fn step(s: &str) {
    let f = ON_STEP.lock().unwrap().clone();
    if let Some(f) = f {
        f(s);
    }
}

/// Part of a stream file that a playlist plays: bytes `start..end` (multiples of 6144) of file `name`.
#[derive(Clone)]
pub struct Seg {
    pub name: String,
    pub start: u64,
    pub end: u64,
}

/// The EP map (CPI) of a clip info file: for the first video stream, (time in 45 kHz ticks, byte offset)
/// of every random access point, in order. Empty when the file has none or cannot be read.
fn ep_map(d: &[u8]) -> Vec<(u64, u64)> {
    let u32at = |p: usize| d.get(p..p + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let u16at = |p: usize| d.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize);
    let go = || -> Option<Vec<(u64, u64)>> {
        let cpi = u32at(16)?;
        if cpi == 0 {
            return None;
        }
        let m = cpi + 6; // EP_map follows the CPI length (4) and type (2)
        let n = *d.get(m + 1)? as usize;
        let e = m
            + 2
            + 12 * (0..n)
                .find(|&i| d.get(m + 2 + 12 * i + 3).is_some_and(|b| (b >> 2) & 0xF == 1))
                .unwrap_or(0);
        let w = u32at(e + 2)?;
        let (coarse_n, fine_n) = ((w >> 2) & 0xFFFF, ((w & 3) << 16) | u16at(e + 6)?);
        let sec = m + u32at(e + 8)?;
        let fine_at = sec + u32at(sec)?;
        let mut out = Vec::with_capacity(fine_n);
        let mut c = 0;
        for f in 0..fine_n {
            while c + 1 < coarse_n && (u32at(sec + 4 + 8 * (c + 1))? >> 14) <= f {
                c += 1;
            }
            let (cw, cs) = (u32at(sec + 4 + 8 * c)?, u32at(sec + 8 + 8 * c)?);
            let fw = u32at(fine_at + 4 * f)?;
            let pts = (((cw & 0x3FFE) as u64) << 18) | ((((fw >> 17) & 0x7FF) as u64) << 8);
            let spn = ((cs as u64) & !0x1FFFF) | (fw & 0x1FFFF) as u64;
            out.push((pts, spn * 192));
        }
        Some(out)
    };
    go().unwrap_or_default()
}

impl Disc {
    /// The byte ranges that playlist clip `c` plays (None for angles other than the first, or when the file is missing).
    pub fn segment(&self, c: &Clip) -> Option<Seg> {
        const U: u64 = 6144;
        if c.angle > 0 || !c.has_file {
            return None;
        }
        let f = self.files.get(&c.name)?;
        let name = f.node.as_ref().map_or(c.name.clone(), |n| n.name.clone());
        let ep = self.eps.get(&c.clpi.to_uppercase()).map_or(&[][..], |v| &v[..]);
        let (tin, tout) = ((c.time_in * 45000.0) as u64, (c.time_out * 45000.0) as u64);
        // start at the last entry point at or before the in time, end at the first one at or after the out time
        let start = ep.iter().rev().find(|e| e.0 <= tin).map_or(0, |e| e.1);
        let end = ep.iter().find(|e| e.0 >= tout).map_or(f.size, |e| e.1);
        let (start, end) = (start / U * U, end.min(f.size).div_ceil(U) * U);
        (end > start).then_some(Seg { name, start, end })
    }
}

pub fn parse_clpi(d: &[u8]) -> Result<StreamSet, String> {
    if d.len() < 16 || !matches!(&d[..8], b"HDMV0100" | b"HDMV0200" | b"HDMV0300") {
        return Err("unknown clip info file type".into());
    }
    let ci = i32be(d, 12) as usize;
    if ci + 4 > d.len() {
        return Err("truncated clip info".into());
    }
    let len = i32be(d, ci) as usize;
    let c = d.get(ci + 4..(ci + 4 + len).min(d.len())).ok_or("truncated clip info")?;
    let mut set = StreamSet::default();
    let mut off = 10;
    for _ in 0..*c.get(8).ok_or("truncated clip info")? {
        if off + 5 > c.len() {
            break;
        }
        let pid = i16be(c, off) as u16;
        off += 2;
        let t = c[off + 1];
        let mut s = None;
        let lang = |p: usize| {
            c.get(off + p..off + p + 3)
                .map(|b| b.iter().map(|&x| x as char).collect::<String>())
        };
        match t {
            HEVC_VIDEO | AVC_VIDEO | MPEG1_VIDEO | MPEG2_VIDEO | VC1_VIDEO => {
                let mut v = Stream::new(pid, t);
                v.set_video_format(c[off + 2] >> 4);
                v.set_frame_rate(c[off + 2] & 0xF);
                v.aspect = c[off + 3] >> 4;
                s = Some(v);
            }
            AC3_AUDIO | AC3_PLUS_AUDIO | AC3_PLUS_SECONDARY | TRUEHD_AUDIO | DTS_AUDIO | DTS_HD_AUDIO | DTS_HD_MASTER_AUDIO
            | DTS_HD_SECONDARY | LPCM_AUDIO | MPEG1_AUDIO | MPEG2_AUDIO | MPEG2_AAC | MPEG4_AAC => {
                let mut a = Stream::new(pid, t);
                a.layout = c[off + 2] >> 4;
                a.sample_rate = Stream::convert_sample_rate(c[off + 2] & 0xF);
                a.set_lang(&lang(3).unwrap_or_default());
                s = Some(a);
            }
            IGS | PGS => {
                let mut g = Stream::new(pid, t);
                g.set_lang(&lang(2).unwrap_or_default());
                s = Some(g);
            }
            SUBTITLE => {
                let mut x = Stream::new(pid, t);
                x.set_lang(&lang(3).unwrap_or_default());
                s = Some(x);
            }
            _ => {}
        }
        if let Some(s) = s {
            set.insert(s);
        }
        off += c[off] as usize + 1;
    }
    Ok(set)
}

fn playlist_stream(d: &[u8], pos: &mut usize) -> Option<u16> {
    let hl = d[*pos] as usize;
    let hp = *pos + 1;
    let ht = d[hp];
    let pid = match ht {
        1 => i16be(d, hp + 1),
        2 | 4 => i16be(d, hp + 3),
        3 => i16be(d, hp + 2),
        _ => 0,
    };
    *pos = hp + hl;
    let sl = d[*pos] as usize;
    let sp = *pos + 1;
    *pos = sp + sl;
    let known = matches!(
        d[sp],
        HEVC_VIDEO
            | AVC_VIDEO
            | MPEG1_VIDEO
            | MPEG2_VIDEO
            | VC1_VIDEO
            | AC3_AUDIO
            | AC3_PLUS_AUDIO
            | AC3_PLUS_SECONDARY
            | TRUEHD_AUDIO
            | DTS_AUDIO
            | DTS_HD_AUDIO
            | DTS_HD_MASTER_AUDIO
            | DTS_HD_SECONDARY
            | LPCM_AUDIO
            | MPEG1_AUDIO
            | MPEG2_AUDIO
            | MPEG2_AAC
            | MPEG4_AAC
            | IGS
            | PGS
            | SUBTITLE
    );
    known.then_some(pid as u16)
}

pub fn parse_mpls(
    name: &str,
    node: &Node,
    clpis: &HashMap<String, StreamSet>,
    files: &HashMap<String, StreamFile>,
) -> Result<Playlist, String> {
    let d = node.read().map_err(|e| e.to_string())?;
    if d.len() < 0x40 {
        return Err("truncated playlist".into());
    }
    let ft = ascii(&d, 0, 8);
    if !matches!(ft.as_str(), "MPLS0100" | "MPLS0200" | "MPLS0300") {
        return Err(format!("unknown file type {ft}"));
    }
    let (po, co) = (i32be(&d, 8) as usize, i32be(&d, 12) as usize);
    let mut p = Playlist {
        name: name.into(),
        ..Default::default()
    };
    let mut pos = po + 4 + 2;
    let items = i16be(&d, pos);
    pos += 4;
    let mut total = 0.0;
    let mut chapter_clips = Vec::new();
    let oob = || "playlist data truncated".to_string();
    for _ in 0..items {
        if pos + 40 > d.len() {
            return Err(oob());
        }
        let start = pos;
        let item_len = i16be(&d, pos);
        let item = ascii(&d, pos + 2, 5);
        pos += 11;
        let fname = format!("{item}.M2TS");
        let clpi = format!("{item}.CLPI");
        if !clpis.contains_key(&clpi) {
            return Err(format!("referenced missing file {clpi}"));
        }
        pos += 1;
        let multi = (d[pos] >> 4) & 1;
        pos += 2;
        let mut t = [0f64; 2];
        for v in &mut t {
            let mut x = i32be(&d, pos);
            if x & 0x8000_0000 != 0 {
                x &= 0x7FFF_FFFF;
            }
            *v = x as f64 / 45000.0;
            pos += 4;
        }
        let len = t[1] - t[0];
        let mk = |name: &str, clpi: &str, angle: usize| Clip {
            name: name.into(),
            clpi: clpi.into(),
            has_file: files.contains_key(name),
            file_size: files.get(name).map_or(0, |f| f.size),
            angle,
            ..Default::default()
        };
        let mut clip = mk(&fname, &clpi, 0);
        clip.time_in = t[0];
        clip.time_out = t[1];
        clip.length = len;
        clip.rel_in = total;
        clip.rel_out = total + len;
        clip.rel_length = len / total;
        total += len;
        let rel_length = clip.rel_length;
        chapter_clips.push(p.clips.len());
        p.clips.push(clip.clone());
        pos += 12;
        if multi > 0 {
            let angles = d[pos] as usize;
            pos += 2;
            for a in 0..angles.saturating_sub(1) {
                let an = ascii(&d, pos, 5);
                pos += 10;
                let (af, ac) = (format!("{an}.M2TS"), format!("{an}.CLPI"));
                if !files.contains_key(&af) {
                    return Err(format!("referenced missing angle file {af}"));
                }
                if !clpis.contains_key(&ac) {
                    return Err(format!("referenced missing angle file {ac}"));
                }
                let mut c = mk(&af, &ac, a + 1);
                c.time_in = clip.time_in;
                c.time_out = clip.time_out;
                c.rel_in = clip.rel_in;
                c.rel_out = clip.rel_out;
                c.length = clip.length;
                p.clips.push(c);
            }
            p.angle_count = p.angle_count.max(angles.saturating_sub(1));
        }
        pos += 2;
        let counts: Vec<usize> = (0..7).map(|i| d[pos + 2 + i] as usize).collect();
        pos += 9 + 5;
        // order: video, audio, PG, IG, secondary audio (+2), secondary video (+6)
        for (i, &n) in counts.iter().take(6).enumerate() {
            for _ in 0..n {
                if let Some(pid) = playlist_stream(&d, &mut pos) {
                    if !p.listed.contains(&pid) || rel_length > 0.01 {
                        p.listed.insert(pid);
                    }
                }
                pos += match i {
                    4 => 2,
                    5 => 6,
                    _ => 0,
                };
            }
        }
        pos = start + item_len + 2;
    }
    pos = co + 4;
    if pos + 2 > d.len() {
        return Err(oob());
    }
    let n = i16be(&d, pos);
    pos += 2;
    for _ in 0..n {
        if pos + 8 > d.len() {
            break;
        }
        if d[pos + 1] == 1 {
            let idx = i16be(&d, pos + 2);
            let ct = i32be(&d, pos + 4) as f64 / 45000.0;
            let ci = *chapter_clips.get(idx).ok_or_else(oob)?;
            let c = &mut p.clips[ci];
            let rel = ct - c.time_in + c.rel_in;
            if total - rel > 1.0 {
                c.chapters.push(ct);
                p.chapters.push(rel);
            }
        }
        pos += 14;
    }
    Ok(p)
}

impl Disc {
    /// Open a BDMV folder (or any folder above it), a UDF image (ISO), or a raw disc device.
    pub fn open(input: &Path) -> Result<Disc, String> {
        step("Opening disc: reading the file system");
        let m = fs::metadata(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let (root, label, disc_path) = if m.is_dir() {
            let input = fs::canonicalize(input).map_err(|e| format!("{}: {e}", input.display()))?;
            let mut bdmv = None;
            let mut d = Some(input.as_path());
            while let Some(x) = d {
                if x.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("BDMV")) {
                    bdmv = Some(x.to_path_buf());
                    break;
                }
                d = x.parent();
            }
            let bdmv = bdmv.or_else(|| find_ci(&input, "BDMV")).ok_or("unable to locate BDMV directory")?;
            let root = bdmv.parent().unwrap_or(&bdmv).to_path_buf();
            let label = root.file_name().map(|n| n.to_string_lossy().into()).unwrap_or_default();
            (Node::fs(&root), label, root)
        } else {
            let (r, l) = vfs::open_udf(input).map_err(|e| format!("{}: {e}", input.display()))?;
            (r, l, input.to_path_buf())
        };
        if let Some(f) = std::env::var_os("BDVIEW_PLUS_LIST") {
            let mut v = Vec::new();
            list_files(&root, "", &mut v);
            v.sort_by(|a, b| a.split_once(' ').map(|x| x.1).cmp(&b.split_once(' ').map(|x| x.1)));
            let _ = fs::write(f, v.join("\n") + "\n");
        }
        let bdmv = root.find("BDMV").ok_or("unable to locate BDMV directory")?;
        let sub = |n: &str| bdmv.find(n);
        let nonempty = |n: Option<Node>| n.is_some_and(|n| !n.children().is_empty());
        let mut disc = Disc {
            source: input.display().to_string(),
            label,
            size: dir_size(&root),
            is_uhd: sub("index.bdmv")
                .and_then(|n| n.read().ok())
                .is_some_and(|ix| ix.starts_with(b"INDX0300")),
            is_bdplus: ["BDSVM", "SLYVM", "ANYVM"].iter().any(|n| root.find(n).is_some()),
            is_bdjava: nonempty(sub("BDJO")),
            is_dbox: root.find("FilmIndex.xml").is_some(),
            is_psp: root.find("SNP").is_some_and(|p| !list_ext(&p, "mnv").is_empty()),
            is_3d: nonempty(sub("STREAM").and_then(|s| s.find("SSIF"))),
            is_50hz: false,
            title: sub("META")
                .and_then(|m| find_meta(&m))
                .and_then(|n| n.read().ok())
                .and_then(|b| xml_title(&String::from_utf8_lossy(&b))),
            clpis: HashMap::new(),
            eps: HashMap::new(),
            files: HashMap::new(),
            playlists: Vec::new(),
            errors: Vec::new(),
            #[cfg(feature = "bluray")]
            bd: None,
        };
        step("Reading clip information");
        if let Some(cd) = sub("CLIPINF") {
            for n in list_ext(&cd, "clpi") {
                let name = n.name.to_uppercase();
                // one read per file: the metadata area is small and contiguous, keep the head moving forward
                match n
                    .read()
                    .map_err(|e| e.to_string())
                    .and_then(|d| parse_clpi(&d).map(|s| (s, ep_map(&d))))
                {
                    Ok((s, e)) => {
                        disc.eps.insert(name.clone(), e);
                        disc.clpis.insert(name, s);
                    }
                    Err(e) => disc.errors.push(format!("{name}: {e}")),
                }
            }
        }
        if let Some(sd) = sub("STREAM") {
            for n in list_ext(&sd, "m2ts") {
                let name = n.name.to_uppercase();
                disc.files.insert(
                    name.clone(),
                    StreamFile {
                        name,
                        size: n.size,
                        node: Some(n),
                        ..Default::default()
                    },
                );
            }
        }
        step("Reading playlists");
        if let Some(pd) = sub("PLAYLIST") {
            for n in list_ext(&pd, "mpls") {
                let name = n.name.to_uppercase();
                match parse_mpls(&name, &n, &disc.clpis, &disc.files) {
                    Ok(pl) => disc.playlists.push(pl),
                    Err(e) => disc.errors.push(format!("{name}: {e}")),
                }
            }
        }
        // all the small metadata files are read by now; only then start the AACS handshake and the probe read
        #[cfg(feature = "bluray")]
        disc.decrypt_if_scrambled(&disc_path);
        #[cfg(not(feature = "bluray"))]
        let _ = disc_path;
        Ok(disc)
    }

    /// Stream files are always read through libbluray (AACS via libaacs or MakeMKV's libmmbd) when it can be loaded;
    /// without it they are read directly, which only works for decrypted data.
    #[cfg(feature = "bluray")]
    fn decrypt_if_scrambled(&mut self, path: &Path) {
        use crate::bluray::{open, scrambled, DISABLE};
        use std::{io::Read, sync::atomic::Ordering};
        let head_of = |n: &Node| {
            let mut head = vec![0u8; 197];
            n.open().and_then(|mut r| r.read_exact(&mut head)).is_ok().then_some(head)
        };
        if DISABLE.load(Ordering::Relaxed) {
            return;
        }
        let probe = self
            .files
            .values()
            .filter(|f| f.size >= 6144)
            .min_by_key(|f| f.size)
            .and_then(|f| f.node.clone());
        let Some(probe) = probe else { return };
        let readable = |h: Option<Vec<u8>>| h.is_some_and(|h| !scrambled(&h));
        // BDVIEW_PLUS_TIMING=1 prints how long each libbluray step takes (to stderr)
        let timing = std::env::var_os("BDVIEW_PLUS_TIMING").is_some();
        let t0 = std::time::Instant::now();
        step("AACS handshake with the drive (MakeMKV libmmbd) - this can take 20 seconds or more");
        let opened = open(path);
        step("Checking that the stream files decrypt");
        if timing {
            eprintln!(
                "bdview-plus: libbluray open: {:.1}s ({})",
                t0.elapsed().as_secs_f32(),
                if opened.is_ok() { "ok" } else { "failed" }
            );
        }
        match opened {
            Ok(bd) => {
                self.bd = Some(bd.clone());
                let t1 = std::time::Instant::now();
                let dec = head_of(&probe.decrypted(&bd));
                if timing {
                    eprintln!(
                        "bdview-plus: first decrypted read: {:.1}s ({})",
                        t1.elapsed().as_secs_f32(),
                        if dec.is_some() { "ok" } else { "failed" }
                    );
                }
                if readable(dec.clone()) {
                    for f in self.files.values_mut() {
                        f.node = f.node.as_ref().map(|n| n.decrypted(&bd));
                    }
                } else if !readable(head_of(&probe)) {
                    // neither route gives usable data
                    self.errors.push(if dec.is_none() {
                        "libbluray could not read the stream files: AACS decryption failed (libaacs or MakeMKV's libmmbd is not set up, or there is no key for this disc)".to_string()
                    } else {
                        "stream files are still scrambled after libbluray: set up AACS decryption (libaacs with a key file, or MakeMKV's libmmbd as libaacs)".to_string()
                    });
                }
            }
            Err(e) if !readable(head_of(&probe)) => self.errors.push(format!("stream files are encrypted: {e}")),
            Err(_) => {}
        }
    }

    /// Sum PGS caption counts over every clip of each selected playlist (UpdateSubtitleChapterCount).
    pub fn update_captions(&mut self, sel: &[usize]) {
        for &pi in sel {
            let p = &mut self.playlists[pi];
            for s in p.streams.list.iter_mut().filter(|s| s.is_graphics()) {
                s.captions = 0;
                s.forced_captions = 0;
            }
            for c in &p.clips {
                let Some(f) = self.files.get(&c.name) else { continue };
                for cs in f.streams.list.iter().filter(|s| s.is_graphics()) {
                    let Some(ps) = p.streams.get_mut(cs.pid) else { continue };
                    ps.captions += cs.captions;
                    ps.forced_captions += cs.forced_captions;
                    if ps.width == 0 && cs.width > 0 {
                        ps.width = cs.width;
                    }
                    if ps.height == 0 && cs.height > 0 {
                        ps.height = cs.height;
                    }
                }
            }
        }
    }

    /// Build a user playlist (BDInfo's "Custom playlist") from clips of existing playlists, given as
    /// (playlist name, clip index). Replaces a playlist of the same name. Returns its index.
    pub fn add_custom(&mut self, name: &str, items: &[(String, usize)]) -> Option<usize> {
        let mut p = Playlist {
            name: name.to_string(),
            custom: true,
            ..Default::default()
        };
        let mut total = 0.0;
        for (pn, ci) in items {
            let Some(src) = self.playlists.iter().find(|q| &q.name == pn) else {
                continue;
            };
            let Some(c) = src.clips.get(*ci) else { continue };
            p.listed.extend(src.listed.iter().copied());
            let mut c = c.clone();
            c.length = c.time_out - c.time_in;
            c.rel_in = total;
            c.rel_out = total + c.length;
            c.chapters = vec![c.time_in];
            c.payload_bytes = 0;
            c.packet_count = 0;
            c.packet_seconds = 0.0;
            p.angle_count = p.angle_count.max(c.angle);
            if c.angle == 0 {
                p.chapters.push(c.rel_in);
                total += c.length;
            }
            p.clips.push(c);
        }
        if p.clips.is_empty() {
            return None;
        }
        for c in p.clips.iter_mut() {
            c.rel_length = if total > 0.0 { c.length / total } else { 0.0 };
        }
        p.initialize(&self.clpis, &self.files);
        self.playlists.retain(|q| q.name != p.name);
        self.playlists.push(p);
        Some(self.playlists.len() - 1)
    }

    /// Finalize playlists after the TS scan.
    pub fn finish(&mut self) {
        let (clpis, files) = (&self.clpis, &self.files);
        for p in &mut self.playlists {
            p.initialize(clpis, files);
        }
        self.refresh_flags();
    }

    /// Like `finish`, but rebuilds only the playlists in `sel` (plus any not built yet), so playlists
    /// scanned earlier keep their measurements.
    pub fn finish_sel(&mut self, sel: &[usize]) {
        let (clpis, files) = (&self.clpis, &self.files);
        for (i, p) in self.playlists.iter_mut().enumerate() {
            if sel.contains(&i) || !p.initialized {
                p.initialize(clpis, files);
            }
        }
        self.refresh_flags();
    }

    pub fn refresh_flags(&mut self) {
        for p in &self.playlists {
            self.is_50hz |= p
                .streams
                .list
                .iter()
                .any(|s| s.is_video() && matches!((s.fr_num, s.fr_den), (25000, 1000) | (50000, 1000)));
        }
    }
}

fn find_meta(dir: &Node) -> Option<Node> {
    for c in dir.children() {
        if c.is_dir {
            if let Some(f) = find_meta(&c) {
                return Some(f);
            }
        } else if c.name.eq_ignore_ascii_case("bdmt_eng.xml") {
            return Some(c);
        }
    }
    None
}

/// Text of <di:discinfo><di:title><di:name>.
fn xml_title(x: &str) -> Option<String> {
    let t = &x[x.find("<di:title>")?..];
    let t = &t[t.find("<di:name>")? + 9..];
    let name = t[..t.find("</di:name>")?].trim();
    let name = name
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    (!name.is_empty() && !name.eq_ignore_ascii_case("blu-ray")).then_some(name)
}
