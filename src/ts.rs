//! M2TS scanner (port of TSStreamFile.Scan): PAT/PMT, PES reassembly, codec dispatch and
//! the per-PID window accounting behind every bitrate figure in the report.

use crate::bits::Bits;
use crate::codecs::{self, Tag};
use crate::disc::{Diag, Playlist, StreamFile};
use crate::hevc;
use crate::stream::*;
use std::collections::HashMap;
use std::io::Read;

const CHUNK: usize = 6 << 20;
/// Quick scans give up after this many bytes even if some stream never initializes.
pub const QUICK_CAP: u64 = 48 << 20;

#[derive(Default)]
struct St {
    window_packets: u64,
    window_bytes: u64,
    transfer: bool,
    variable: bool,
    remain: i64,
    skip: usize,
    buf: Vec<u8>,
    tlen: usize,
    pts_last: i64,
    pts_transfer: i64,
    dts_prev: i64,
    pts_count: u64,
    tag: Tag,
}

#[derive(Default)]
struct Psi {
    buf: Vec<u8>,
    active: bool,
}

struct Ctx<'a> {
    f: &'a mut StreamFile,
    pls: &'a mut Vec<Playlist>,
    targets: Vec<(usize, usize)>,
    full: bool,
    pids: Vec<u16>,
    index: HashMap<u16, usize>,
    states: Vec<St>,
    pmt_pid: u16,
    pat: Psi,
    pmt: Psi,
    pts_first: i64,
    pts_max: i64,
}

fn pts(b: &[u8]) -> i64 {
    (((b[0] >> 1) as i64 & 7) << 30) | ((b[1] as i64) << 22) | (((b[2] >> 1) as i64) << 15) | ((b[3] as i64) << 7) | (b[4] >> 1) as i64
}

fn header_ok(s: &Stream, id: u8) -> bool {
    let video = id == 0xFD || (0xE0..=0xEF).contains(&id);
    let audio = id == 0xBD || (0xC0..=0xDF).contains(&id) || id == 0xFA || id == 0xFD;
    if s.is_video() {
        video
    } else if s.is_audio() {
        audio
    } else {
        matches!(id, 0xFA | 0xFD | 0xBD) || (0xE0..=0xEF).contains(&id)
    }
}

/// Scan one stream file. `sel` lists the playlists (indices into `pls`) that reference it.
/// Returns an error only for I/O failures.
pub fn scan_file(
    f: &mut StreamFile,
    pls: &mut Vec<Playlist>,
    sel: &[usize],
    full: bool,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<(), String> {
    let mut file = f.node.as_ref().ok_or("no file")?.open().map_err(|e| format!("{}: {e}", f.name))?;
    f.streams.clear();
    f.diag.clear();
    f.length = 0.0;
    let targets: Vec<(usize, usize)> = if full {
        sel.iter().flat_map(|&p| pls[p].clips.iter().enumerate().filter(|(_, c)| c.name == f.name).map(move |(i, _)| (p, i))).collect()
    } else {
        Vec::new()
    };
    let mut c = Ctx {
        f,
        pls,
        targets,
        full,
        pids: Vec::new(),
        index: HashMap::new(),
        states: Vec::new(),
        pmt_pid: 0xFFFF,
        pat: Psi::default(),
        pmt: Psi::default(),
        pts_first: i64::MAX,
        pts_max: 0,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK + 400);
    let mut chunk = vec![0u8; CHUNK];
    let mut total = 0u64;
    let mut valid = 0u64;
    let no_data = |total: u64, valid: u64| match (total, valid) {
        (0, _) => Err("no data could be read from the file".to_string()),
        (_, 0) => Err("no transport stream found (the data is probably still encrypted)".to_string()),
        _ => Ok(()),
    };
    loop {
        let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        buf.extend_from_slice(&chunk[..n]);
        let mut pos = 0;
        while pos + 192 <= buf.len() {
            if buf[pos + 4] != 0x47 {
                pos += 1;
                continue;
            }
            valid += 1;
            if c.packet(&buf[pos + 4..pos + 192]) {
                return Ok(());
            }
            pos += 192;
        }
        buf.drain(..pos);
        if !progress(total) {
            return Err("cancelled".into());
        }
        if !full && total >= QUICK_CAP {
            return no_data(total, valid);
        }
    }
    no_data(total, valid)?;
    c.finish(sel);
    Ok(())
}

impl Ctx<'_> {
    fn state(&mut self, pid: u16) -> usize {
        if let Some(&i) = self.index.get(&pid) {
            return i;
        }
        self.pids.push(pid);
        self.states.push(St::default());
        self.index.insert(pid, self.states.len() - 1);
        self.states.len() - 1
    }

    /// Process one 188-byte TS packet. Returns true when a quick scan has seen enough.
    fn packet(&mut self, h: &[u8]) -> bool {
        let pusi = h[1] & 0x40 != 0;
        let pid = (((h[1] & 0x1F) as u16) << 8) | h[2] as u16;
        let scrambled = (h[3] >> 6) & 3 != 0;
        let afc = (h[3] >> 4) & 3;
        let idx = self.state(pid);
        self.states[idx].window_packets += 1;
        if pusi && pid != 0 && pid != self.pmt_pid && self.states[idx].transfer {
            self.states[idx].transfer = false;
            if self.scan_stream(idx) && !self.full {
                return true;
            }
        }
        let mut off = 4;
        if afc & 2 != 0 {
            off += 1 + h[4] as usize;
        }
        let payload = if off < 188 { &h[off..] } else { &[][..] };
        if pid == 0 {
            if let Some(sec) = Self::psi(&mut self.pat, pusi, payload) {
                self.pat_section(&sec);
            }
        } else if pid == self.pmt_pid {
            if let Some(sec) = Self::psi(&mut self.pmt, pusi, payload) {
                self.pmt_section(&sec);
            }
        } else if self.f.streams.contains(pid) && !scrambled && !payload.is_empty() {
            return self.pes(idx, pid, pusi, payload) && !self.full;
        }
        false
    }

    /// Accumulate a PSI section; returns it once complete.
    fn psi(p: &mut Psi, pusi: bool, payload: &[u8]) -> Option<Vec<u8>> {
        if pusi {
            let start = 1 + *payload.first()? as usize;
            if start >= payload.len() {
                p.active = false;
                return None;
            }
            p.buf.clear();
            p.buf.extend_from_slice(&payload[start..]);
            p.active = true;
        } else if p.active {
            p.buf.extend_from_slice(payload);
        } else {
            return None;
        }
        if p.buf.len() < 3 {
            return None;
        }
        let want = 3 + ((((p.buf[1] & 0xF) as usize) << 8) | p.buf[2] as usize);
        if want > 3 + 1021 {
            p.active = false;
            return None;
        }
        if p.buf.len() >= want {
            p.active = false;
            return Some(p.buf[..want].to_vec());
        }
        None
    }

    fn pat_section(&mut self, s: &[u8]) {
        if s.len() < 12 || s[6] != s[7] {
            return;
        }
        for e in s[8..s.len() - 4].chunks_exact(4) {
            if ((e[0] as u16) << 8 | e[1] as u16) == 1 {
                self.pmt_pid = (((e[2] & 0x1F) as u16) << 8) | e[3] as u16;
            }
        }
    }

    fn pmt_section(&mut self, s: &[u8]) {
        if s.len() < 16 || s[0] != 2 || s[6] != s[7] {
            return;
        }
        let end = s.len() - 4;
        let mut k = 12 + ((((s[10] & 0xF) as usize) << 8) | s[11] as usize);
        while k + 5 <= end {
            let t = s[k];
            let pid = (((s[k + 1] & 0x1F) as u16) << 8) | s[k + 2] as u16;
            let info = (((s[k + 3] & 0xF) as usize) << 8) | s[k + 4] as usize;
            let known = matches!(
                t,
                MVC_VIDEO | AVC_VIDEO | HEVC_VIDEO | MPEG1_VIDEO | MPEG2_VIDEO | VC1_VIDEO | AC3_AUDIO | AC3_PLUS_AUDIO
                    | AC3_PLUS_SECONDARY | TRUEHD_AUDIO | DTS_AUDIO | DTS_HD_AUDIO | DTS_HD_MASTER_AUDIO | DTS_HD_SECONDARY
                    | LPCM_AUDIO | MPEG1_AUDIO | MPEG2_AUDIO | MPEG2_AAC | MPEG4_AAC | IGS | PGS | SUBTITLE
            );
            if known && !self.f.streams.contains(pid) {
                let mut st = Stream::new(pid, t);
                if st.is_graphics() {
                    st.initialized = !self.full;
                }
                self.f.streams.insert(st);
            }
            self.f.diag.entry(pid).or_default();
            k += 5 + info;
        }
    }

    /// Handle the payload of a TS packet belonging to an elementary stream. True = quick scan done.
    fn pes(&mut self, idx: usize, pid: u16, pusi: bool, payload: &[u8]) -> bool {
        if self.states[idx].transfer {
            let st = &mut self.states[idx];
            let k = st.skip.min(payload.len());
            st.skip -= k;
            return self.feed(idx, pid, &payload[k..]);
        }
        if !pusi || payload.len() < 9 || payload[..3] != [0, 0, 1] {
            return false;
        }
        let s = self.f.streams.get(pid).unwrap();
        if !header_ok(s, payload[3]) {
            return false;
        }
        let is_video = s.is_video();
        let pes_len = ((payload[4] as i64) << 8) | payload[5] as i64;
        let flags = payload[7];
        let hl = payload[8] as usize;
        let st = &mut self.states[idx];
        st.buf.clear();
        st.tlen = 0;
        st.remain = pes_len - 3 - hl as i64;
        st.variable = pes_len == 0 || st.remain <= 0;
        let t = |o: usize| payload.get(9 + o..14 + o).map(pts);
        match flags & 0xC0 {
            0x80 if hl >= 5 => {
                if let Some(p) = t(0) {
                    if p != st.pts_last {
                        if st.pts_last > 0 {
                            st.pts_transfer = p - st.pts_last;
                        }
                        st.pts_last = p;
                    }
                    let diff = p - st.dts_prev;
                    let count = st.pts_count;
                    st.dts_prev = p;
                    st.pts_count += 1;
                    if count > 0 && is_video {
                        self.update_bitrates(pid, p, diff);
                        self.track(p);
                    }
                }
            }
            0xC0 if hl >= 10 => {
                if let (Some(p), Some(d)) = (t(0), t(5)) {
                    if p > st.pts_last {
                        st.pts_last = p;
                    }
                    let diff = d - st.dts_prev;
                    let count = st.pts_count;
                    st.dts_prev = d;
                    st.pts_count += 1;
                    if count > 0 && is_video {
                        self.update_bitrates(pid, d, diff);
                        self.track(d);
                    }
                }
            }
            _ => {}
        }
        let st = &mut self.states[idx];
        st.transfer = true;
        let off = 9 + hl;
        st.skip = off.saturating_sub(payload.len());
        self.feed(idx, pid, payload.get(off..).unwrap_or(&[]))
    }

    fn track(&mut self, t: i64) {
        self.pts_first = self.pts_first.min(t);
        self.pts_max = self.pts_max.max(t);
        self.f.length = (self.pts_max - self.pts_first) as f64 / 90000.0;
    }

    fn feed(&mut self, idx: usize, pid: u16, d: &[u8]) -> bool {
        let s = self.f.streams.get(pid).unwrap();
        let keep = !s.initialized || s.is_video() || s.is_graphics();
        let cap = if s.initialized && s.is_video() { 16 << 10 } else { 64 << 20 };
        let st = &mut self.states[idx];
        let take = if st.variable { d.len() } else { d.len().min(st.remain.max(0) as usize) };
        if keep && st.buf.len() < cap {
            let n = take.min(cap - st.buf.len());
            st.buf.extend_from_slice(&d[..n]);
        }
        st.tlen += take;
        st.window_bytes += take as u64;
        if !st.variable {
            st.remain -= take as i64;
            if st.remain <= 0 {
                st.transfer = false;
                return self.scan_stream(idx);
            }
        }
        false
    }

    /// Run the codec scanner on the buffered PES. True when every stream is initialized.
    fn scan_stream(&mut self, idx: usize) -> bool {
        let pid = self.pids[idx];
        let full = self.full;
        let st = &mut self.states[idx];
        st.tag = None;
        let s = self.f.streams.get_mut(pid).unwrap();
        let bitrate = if s.is_audio() && st.pts_transfer > 0 {
            rnd(st.tlen as f64 * 8.0 / (st.pts_transfer as f64 / 90000.0)) as i64
        } else {
            0
        };
        let mut b = Bits::new(std::mem::take(&mut st.buf));
        let tag = &mut st.tag;
        match s.stype {
            MPEG2_VIDEO => codecs::mpeg2(s, &mut b, tag),
            AVC_VIDEO => codecs::avc(s, &mut b, tag),
            MVC_VIDEO => codecs::mvc(s),
            HEVC_VIDEO => hevc::scan(s, &mut b, tag),
            VC1_VIDEO => codecs::vc1(s, &mut b, tag),
            MPEG1_AUDIO | MPEG2_AUDIO => codecs::mpa(s, &mut b),
            MPEG2_AAC | MPEG4_AAC => codecs::aac(s, &mut b),
            AC3_AUDIO | AC3_PLUS_AUDIO | AC3_PLUS_SECONDARY => codecs::ac3(s, &mut b),
            TRUEHD_AUDIO => codecs::truehd(s, &mut b, tag),
            LPCM_AUDIO => codecs::lpcm(s, &mut b),
            DTS_AUDIO => codecs::dts(s, &mut b, bitrate),
            DTS_HD_AUDIO | DTS_HD_MASTER_AUDIO | DTS_HD_SECONDARY => codecs::dtshd(s, &mut b, bitrate, tag),
            PGS if full => codecs::pgs(s, &mut b, tag),
            _ => s.initialized = true,
        }
        let mut data = b.data;
        data.clear();
        st.buf = data;
        st.tlen = 0;
        let l = &self.f.streams.list;
        if l.iter().any(|s| !s.initialized) {
            return false;
        }
        let avc = l.iter().any(|s| s.stype == AVC_VIDEO);
        let mvc = l.iter().any(|s| s.stype == MVC_VIDEO);
        !(mvc && !avc)
    }

    fn update_bitrates(&mut self, ptspid: u16, p: i64, diff: i64) {
        for idx in 0..self.states.len() {
            let pid = self.pids[idx];
            if pid != ptspid && self.f.streams.get(pid).is_some_and(Stream::is_video) {
                continue;
            }
            if self.states[idx].window_packets == 0 {
                continue;
            }
            self.update_bitrate(idx, p, diff);
        }
    }

    fn update_bitrate(&mut self, idx: usize, p: i64, diff: i64) {
        let pid = self.pids[idx];
        let st = &mut self.states[idx];
        let (wb, wp) = (st.window_bytes, st.window_packets);
        let time = p as f64 / 90000.0;
        let interval = diff as f64 / 90000.0;
        let offset = time + interval;
        for &(pi, ci) in &self.targets {
            let pl = &mut self.pls[pi];
            let c = &mut pl.clips[ci];
            if !(time == 0.0 || (time >= c.time_in && time <= c.time_out)) {
                continue;
            }
            c.payload_bytes += wb;
            c.packet_count += wp;
            if offset > c.time_in && offset - c.time_in > c.packet_seconds {
                c.packet_seconds = offset - c.time_in;
            }
            if c.angle > 0 {
                continue;
            }
            if let Some(s) = pl.streams.get_mut(pid) {
                s.payload_bytes += wb;
                s.packet_count += wp;
                if s.is_video() {
                    s.packet_seconds += interval;
                    if s.packet_seconds > 0.0 {
                        s.active_bitrate = rnd(s.payload_bytes as f64 * 8.0 / s.packet_seconds) as i64;
                    }
                }
                if s.stype == TRUEHD_AUDIO {
                    if let Some(core) = &s.core {
                        s.active_bitrate -= core.bitrate;
                    }
                }
            }
        }
        if let Some(s) = self.f.streams.get_mut(pid) {
            s.payload_bytes += wb;
            s.packet_count += wp;
            if s.is_video() {
                self.f.diag.entry(pid).or_default().push(Diag {
                    marker: time,
                    interval,
                    bytes: wb,
                    tag: st.tag.unwrap_or(""),
                });
                s.packet_seconds += interval;
            }
        }
        st.window_packets = 0;
        st.window_bytes = 0;
    }

    /// Flush the remaining windows and refresh VBR playlist bitrates.
    fn finish(&mut self, sel: &[usize]) {
        let (mut last, mut diff) = (0i64, 0i64);
        let vids: Vec<u16> = self.f.streams.list.iter().filter(|s| s.is_video()).map(|s| s.pid).collect();
        for pid in vids {
            if let Some(&i) = self.index.get(&pid) {
                if self.states[i].pts_last > last {
                    last = self.states[i].pts_last;
                    diff = last - self.states[i].dts_prev;
                }
            }
            self.update_bitrates(pid, last, diff);
        }
        for &pi in sel {
            let pl = &mut self.pls[pi];
            let secs: f64 = pl.clips.iter().filter(|c| c.angle == 0).map(|c| c.packet_seconds).sum();
            if secs > 0.0 {
                for s in pl.streams.list.iter_mut().filter(|s| s.is_vbr) {
                    s.bitrate = rnd(s.payload_bytes as f64 * 8.0 / secs) as i64;
                }
            }
        }
    }
}
