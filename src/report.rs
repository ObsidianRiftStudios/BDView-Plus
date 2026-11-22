//! Report generation (port of FormReport.cs). Lines end with "\n".

use crate::disc::{Disc, Playlist};
use crate::stream::*;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

pub struct Options {
    pub diag: bool,
    pub summary: bool,
    pub warnings: Vec<String>,
}

/// Thousands-separated integer ("N0").
pub fn n0(v: i64) -> String {
    let s = v.unsigned_abs().to_string();
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            o.push(',');
        }
        o.push(c);
    }
    if v < 0 { format!("-{o}") } else { o }
}

fn nf(x: f64) -> String {
    n0(rnd(x) as i64)
}

/// TimeSpan-style "h:mm:ss.mmm" (hour component only, like TimeSpan.Hours); `hw` is the hour width.
fn ts(secs: f64, hw: usize) -> String {
    let ticks = (secs * 1e7) as i64;
    let ms = ticks / 10_000;
    format!("{:0hw$}:{:02}:{:02}.{:03}", (ms / 3_600_000) % 24, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

fn mbps(bits_per_sec: f64) -> String {
    format!("{:.2}", rnd(bits_per_sec / 10000.0) / 100.0)
}

pub fn kbps_int(bits_per_sec: i64) -> i64 {
    rnd(bits_per_sec as f64 / 1000.0) as i64
}

fn audio_brief(s: &Stream) -> String {
    let mut a = format!("{} {}", s.codec_alt_name(), s.channel_description());
    if s.bitrate > 0 {
        let _ = write!(a, " {}Kbps", kbps_int(s.bitrate));
    }
    if s.sample_rate > 0 && s.bit_depth > 0 {
        let _ = write!(a, " ({}kHz/{}-bit)", rnd(s.sample_rate as f64 / 1000.0) as i64, s.bit_depth);
    }
    a
}

struct Window {
    bits: VecDeque<f64>,
    secs: VecDeque<f64>,
    bits_sum: f64,
    secs_sum: f64,
    peak: f64,
    loc: f64,
    limit: f64,
}

impl Window {
    fn new(limit: f64) -> Self {
        Window { bits: VecDeque::new(), secs: VecDeque::new(), bits_sum: 0.0, secs_sum: 0.0, peak: 0.0, loc: 0.0, limit }
    }
    fn reset(&mut self) {
        *self = Window::new(self.limit);
    }
    fn add(&mut self, bits: f64, secs: f64, pos: f64) {
        self.secs_sum += secs;
        self.secs.push_back(secs);
        self.bits_sum += bits;
        self.bits.push_back(bits);
        if self.secs_sum > self.limit {
            let rate = self.bits_sum / self.secs_sum;
            if rate > self.peak && pos - self.secs_sum > 0.0 {
                self.peak = rate;
                self.loc = pos - self.secs_sum;
            }
            self.bits_sum -= self.bits.pop_front().unwrap_or(0.0);
            self.secs_sum -= self.secs.pop_front().unwrap_or(0.0);
        }
    }
}

pub fn generate(disc: &Disc, sel: &[usize], o: &Options) -> String {
    let mut r = String::new();
    let protection = if disc.is_bdplus { "BD+" } else if disc.is_uhd { "AACS2" } else { "AACS" };
    let mut extras = Vec::new();
    for (f, n) in [
        (disc.is_uhd, "Ultra HD"),
        (disc.is_bdjava, "BD-Java"),
        (disc.is_50hz, "50Hz Content"),
        (disc.is_3d, "Blu-ray 3D"),
        (disc.is_dbox, "D-BOX Motion Code"),
        (disc.is_psp, "PSP Digital Copy"),
    ] {
        if f {
            extras.push(n);
        }
    }
    let version = format!("TuxInfo {}", env!("CARGO_PKG_VERSION"));
    let mut info = String::new();
    if let Some(t) = &disc.title {
        let _ = writeln!(info, "{:<16}{}", "Disc Title:", t);
    }
    let _ = writeln!(info, "{:<16}{}", "Disc Label:", disc.label);
    let _ = writeln!(info, "{:<16}{} bytes", "Disc Size:", n0(disc.size as i64));
    let _ = writeln!(info, "{:<16}{}", "Protection:", protection);
    if !extras.is_empty() {
        let _ = writeln!(info, "{:<16}{}", "Extras:", extras.join(", "));
    }
    let _ = writeln!(info, "{:<16}{}", "TuxInfo:", version);

    r += &info;
    r += "\n";
    let _ = writeln!(r, "{:<16}", "Notes:");
    if !crate::app_info::REPO_URL.is_empty() {
        let _ = write!(r, "\nTUXINFO HOME:\n  {}\n\n", crate::app_info::REPO_URL);
    } else {
        r += "\n";
    }
    r += "INCLUDES FORUMS REPORT FOR:\n  AVS Forum Blu-ray Audio and Video Specifications Thread\n    http://www.avsforum.com/avs-vb/showthread.php?t=1155731\n\n";
    if !o.warnings.is_empty() {
        r += "WARNING: File errors were encountered during scan:\n";
        for w in &o.warnings {
            let _ = write!(r, "\n{w}\n");
        }
    }

    for &pi in sel {
        let p = &disc.playlists[pi];
        playlist_report(&mut r, disc, p, &info, protection, &extras, o);
    }
    r
}

fn playlist_report(r: &mut String, disc: &Disc, p: &Playlist, info: &str, protection: &str, extras: &[&str], o: &Options) {
    let _ = extras;
    let total_len = p.total_length();
    let total_length = ts(total_len, 1);
    let total_short = ts(total_len, 1);
    let total_short = &total_short[..total_short.rfind('.').unwrap_or(total_short.len())];
    let total_size = n0(p.total_size() as i64);
    let total_bitrate = mbps(p.total_bitrate() as f64);
    let disc_size = n0(disc.size as i64);
    let sorted = p.sorted();
    let video: Vec<&&Stream> = sorted.iter().filter(|s| s.is_video()).collect();
    let audio: Vec<&&Stream> = sorted.iter().filter(|s| s.is_audio()).collect();
    let (video_codec, video_bitrate) = match video.first() {
        Some(v) => (v.codec_alt_name(), mbps(v.bitrate as f64)),
        None => (String::new(), String::new()),
    };
    let (mut audio1, mut audio2, mut lang1) = (String::new(), String::new(), String::new());
    if let Some(a) = audio.first() {
        lang1 = a.lang.clone();
        audio1 = audio_brief(a);
    }
    for a in audio.iter().skip(1) {
        if a.lang == lang1 && a.stype != AC3_PLUS_SECONDARY && a.stype != DTS_HD_SECONDARY && !(a.stype == AC3_AUDIO && a.channels == 2) {
            audio2 = audio_brief(a);
            break;
        }
    }

    let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str, g: &str, h: &str, i: &str| {
        format!("{a:<64}{b:<8}{c:<8}{d:<16}{e:<16}{f:<8}{g:<8}{h:<42}{i}\n")
    };
    let _ = write!(r, "\n********************\nPLAYLIST: {}\n********************\n\n", p.name);
    *r += "<--- BEGIN FORUMS PASTE --->\n[code]\n";
    *r += &row("", "", "", "", "", "Total", "Video", "", "");
    *r += &row("Title", "Codec", "Length", "Movie Size", "Disc Size", "Bitrate", "Bitrate", "Main Audio Track", "Secondary Audio Track");
    *r += &row("-----", "------", "-------", "--------------", "--------------", "-------", "-------", "------------------", "---------------------");
    *r += &row(
        &p.name,
        &video_codec,
        total_short,
        &total_size,
        &disc_size,
        &total_bitrate,
        &video_bitrate,
        &audio1,
        &audio2,
    );
    *r += "[/code]\n\n[code]\n\nDISC INFO:\n\n";
    *r += info;
    *r += "\nPLAYLIST REPORT:\n\n";
    let _ = writeln!(r, "{:<24}{}", "Name:", p.name);
    let _ = writeln!(r, "{:<24}{} (h:m:s.ms)", "Length:", total_length);
    let _ = writeln!(r, "{:<24}{} bytes", "Size:", total_size);
    let _ = writeln!(r, "{:<24}{} Mbps", "Total Bitrate:", total_bitrate);

    let mut summary = String::new();
    if let Some(t) = &disc.title {
        let _ = writeln!(summary, "Disc Title: {}", t);
    }
    let _ = writeln!(summary, "Disc Label: {}", disc.label);
    let _ = writeln!(summary, "Disc Size: {} bytes", disc_size);
    let _ = writeln!(summary, "Protection: {}", protection);
    let _ = writeln!(summary, "Playlist: {}", p.name);
    let _ = writeln!(summary, "Size: {} bytes", total_size);
    let _ = writeln!(summary, "Length: {}", total_length);
    let _ = writeln!(summary, "Total Bitrate: {} Mbps", total_bitrate);

    if p.has_hidden_tracks {
        *r += "\n(*) Indicates included stream hidden by this playlist.\n";
    }
    let star = |s: &Stream| if s.hidden { "* " } else { "" };

    if !video.is_empty() {
        *r += "\nVIDEO:\n\n";
        let _ = writeln!(r, "{:<24}{:<20}{:<16}", "Codec", "Bitrate", "Description");
        let _ = writeln!(r, "{:<24}{:<20}{:<16}", "-----", "-------", "-----------");
        for s in &video {
            let name = s.codec_name();
            let br = format!("{} kbps", kbps_int(s.bitrate));
            let _ = writeln!(r, "{:<24}{:<20}{:<16}", format!("{}{}", star(s), name), br, s.description());
            let _ = writeln!(summary, "{}Video: {} / {} / {}", star(s), name, br, s.description());
        }
    }
    if !audio.is_empty() {
        *r += "\nAUDIO:\n\n";
        let _ = writeln!(r, "{:<32}{:<16}{:<16}{:<16}", "Codec", "Language", "Bitrate", "Description");
        let _ = writeln!(r, "{:<32}{:<16}{:<16}{:<16}", "-----", "--------", "-------", "-----------");
        for s in &audio {
            let br = format!("{} kbps", kbps_int(s.bitrate));
            let _ = writeln!(r, "{:<32}{:<16}{:<16}{:<16}", format!("{}{}", star(s), s.codec_name()), s.lang_name, br, s.description());
            let _ = writeln!(summary, "{}Audio: {} / {} / {}", star(s), s.lang_name, s.codec_name(), s.description());
        }
    }
    for (title, kind) in [("SUBTITLES", 0), ("TEXT", 1)] {
        let list: Vec<&&Stream> = sorted.iter().filter(|s| if kind == 0 { s.is_graphics() } else { s.is_text() }).collect();
        if list.is_empty() {
            continue;
        }
        let _ = write!(r, "\n{title}:\n\n");
        let _ = writeln!(r, "{:<32}{:<16}{:<16}{:<16}", "Codec", "Language", "Bitrate", "Description");
        let _ = writeln!(r, "{:<32}{:<16}{:<16}{:<16}", "-----", "--------", "-------", "-----------");
        for s in list {
            let br = format!("{:.3} kbps", s.bitrate as f64 / 1000.0);
            let _ = writeln!(r, "{:<32}{:<16}{:<16}{:<16}", format!("{}{}", star(s), s.codec_name()), s.lang_name, br, s.description());
            if kind == 0 {
                let _ = writeln!(summary, "{}Subtitle: {} / {}", star(s), s.lang_name, br);
            }
        }
    }

    *r += "\nFILES:\n\n";
    let _ = writeln!(r, "{:<16}{:<16}{:<16}{:<16}{:<16}", "Name", "Time In", "Length", "Size", "Total Bitrate");
    let _ = writeln!(r, "{:<16}{:<16}{:<16}{:<16}{:<16}", "----", "-------", "------", "----", "-------------");
    for c in &p.clips {
        let mut name = c.name.clone();
        if c.angle > 0 {
            let _ = write!(name, " ({})", c.angle);
        }
        let _ = writeln!(
            r,
            "{:<16}{:<16}{:<16}{:<16}{:<16}",
            name,
            ts(c.rel_in, 1),
            ts(c.length, 1),
            n0(c.packet_size() as i64),
            n0(rnd(c.packet_bitrate() as f64 / 1000.0) as i64)
        );
    }

    chapters(r, disc, p, video.first().map(|v| v.pid));

    if o.diag {
        *r += "\nSTREAM DIAGNOSTICS:\n\n";
        let hdr = |r: &mut String, v: [&str; 9]| {
            let _ = writeln!(r, "{:<16}{:<16}{:<16}{:<16}{:<24}{:<24}{:<24}{:<16}{:<16}", v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]);
        };
        hdr(r, ["File", "PID", "Type", "Codec", "Language", "Seconds", "Bitrate", "Bytes", "Packets"]);
        hdr(r, ["----", "---", "----", "-----", "--------", "--------------", "--------------", "-------------", "-----"]);
        let mut seen = HashSet::new();
        for c in &p.clips {
            let Some(f) = disc.files.get(&c.name) else { continue };
            if !seen.insert(c.name.clone()) {
                continue;
            }
            let mut name = c.name.clone();
            if c.angle > 0 {
                let _ = write!(name, " ({})", c.angle);
            }
            for cs in &f.streams.list {
                let Some(ps) = p.streams.get(cs.pid) else { continue };
                let (secs, rate) = if f.length > 0.0 {
                    (format!("{:.3}", f.length), n0(rnd(cs.payload_bytes as f64 * 8.0 / f.length / 1000.0) as i64))
                } else {
                    ("0".to_string(), "0".to_string())
                };
                let lang = if ps.lang.is_empty() { String::new() } else { format!("{} ({})", ps.lang, ps.lang_name) };
                hdr(
                    r,
                    [
                        &name,
                        &format!("{} (0x{:X})", cs.pid, cs.pid),
                        &format!("0x{:02X}", cs.stype),
                        &cs.codec_short_name(),
                        &lang,
                        &secs,
                        &rate,
                        &n0(cs.payload_bytes as i64),
                        &n0(cs.packet_count as i64),
                    ],
                );
            }
        }
    }

    *r += "\n[/code]\n<---- END FORUMS PASTE ---->\n\n";
    if o.summary {
        *r += "QUICK SUMMARY:\n\n";
        *r += &summary;
        *r += "\n";
    }
}

/// Per-chapter bitrate table built from the video diagnostics (same walk as the original).
fn chapters(r: &mut String, disc: &Disc, p: &Playlist, diag_pid: Option<u16>) {
    *r += "\nCHAPTERS:\n\n";
    let cols = |v: [&str; 13]| v.iter().fold(String::new(), |a, s| a + &format!("{s:<16}")) + "\n";
    *r += &cols(["Number", "Time In", "Length", "Avg Video Rate", "Max 1-Sec Rate", "Max 1-Sec Time", "Max 5-Sec Rate", "Max 5-Sec Time", "Max 10Sec Rate", "Max 10Sec Time", "Avg Frame Size", "Max Frame Size", "Max Frame Time"]);
    *r += &cols(["------", "-------", "------", "--------------", "--------------", "--------------", "--------------", "--------------", "--------------", "--------------", "--------------", "--------------", "--------------"]);
    let Some(pid) = diag_pid else { return };
    let total = p.total_length();
    let mut w = [Window::new(1.0), Window::new(5.0), Window::new(10.0)];
    let (mut pos, mut ch_bits, mut frames, mut max_frame, mut max_loc) = (0.0f64, 0.0f64, 0i64, 0.0f64, 0.0f64);
    let (mut ci, mut di, mut chi) = (0usize, 0usize, 0usize);
    while chi < p.chapters.len() {
        let clip = p.clips.get(ci);
        let start = p.chapters[chi];
        let end = if chi + 1 < p.chapters.len() { p.chapters[chi + 1] } else { total };
        let len = end - start;
        let mut list = None;
        if let Some(c) = clip {
            if c.angle == 0 {
                list = disc.files.get(&c.name).and_then(|f| f.diag.get(&pid));
            }
        }
        if let (Some(c), Some(l)) = (clip, list) {
            while di < l.len() && pos < end {
                let d = &l[di];
                di += 1;
                if d.marker < c.time_in {
                    continue;
                }
                pos = d.marker - c.time_in + c.rel_in;
                let bits = d.bytes as f64 * 8.0;
                ch_bits += bits;
                if !d.tag.is_empty() {
                    frames += 1;
                }
                if bits > max_frame * 8.0 {
                    max_frame = bits / 8.0;
                    max_loc = pos;
                }
                for x in &mut w {
                    x.add(bits, d.interval, pos);
                }
            }
        }
        if list.is_none_or(|l| di == l.len()) {
            if ci < p.clips.len() {
                ci += 1;
                di = 0;
            } else {
                pos = end;
            }
        }
        if pos >= end {
            chi += 1;
            let rate = |x: &Window| format!("{} kbps", nf(x.peak / 1000.0));
            let avg = if frames > 0 { ch_bits / frames as f64 / 8.0 } else { 0.0 };
            let line = cols([
                &chi.to_string(),
                &ts(start, 1),
                &ts(len, 1),
                &format!("{} kbps", nf(if len > 0.0 { ch_bits / len / 1000.0 } else { 0.0 })),
                &rate(&w[0]),
                &ts(w[0].loc, 2),
                &rate(&w[1]),
                &ts(w[1].loc, 2),
                &rate(&w[2]),
                &ts(w[2].loc, 2),
                &format!("{} bytes", nf(avg)),
                &format!("{} bytes", nf(max_frame)),
                &ts(max_loc, 2),
            ]);
            *r += &line;
            w.iter_mut().for_each(Window::reset);
            ch_bits = 0.0;
            frames = 0;
            max_frame = 0.0;
            max_loc = 0.0;
        }
    }
}
