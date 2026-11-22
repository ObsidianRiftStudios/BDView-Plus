//! HEVC scanner: SPS / PPS / slice / SEI parsing (port of TSCodecHEVC.cs, VUI parsed partially).

use crate::bits::{unescape, Bits};
use crate::codecs::Tag;
use crate::stream::Stream;
use std::sync::atomic::{AtomicBool, Ordering};

/// Wait for HDR SEI data before declaring an HDR10 stream initialized.
pub static EXTENDED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct Sps {
    profile: u32,
    high_tier: bool,
    level: u32,
    chroma: u32,
    bit_depth: u32,
    full_range: bool,
    primaries: u32,
    transfer: u32,
    matrix: u32,
    timing: Option<(u32, u32)>,
}

#[derive(Clone, Default)]
pub struct Pps {
    extra_bits: usize,
}

#[derive(Clone, Default)]
pub struct Hevc {
    pub sps: Vec<Option<Sps>>,
    pub pps: Vec<Option<Pps>>,
    pub md_primaries: [u16; 8],
    pub md_lum: (u32, u32),
    pub cll: u16,
    pub fall: u16,
    pub cll_avail: bool,
    pub hdr10plus: bool,
    md_avail: bool,
}

fn st_rps(b: &mut Bits, idx: usize, num: usize, nd: &mut Vec<u32>) {
    let inter = idx != 0 && b.bool();
    let count = if inter {
        if idx == num {
            b.skip_ue();
        }
        b.skip(1);
        b.skip_ue();
        let r = nd[idx - 1];
        let mut c = 0;
        for _ in 0..=r {
            let used = b.bool();
            if used || b.bool() {
                c += 1;
            }
        }
        c
    } else {
        let (neg, pos) = (b.ue(), b.ue());
        for _ in 0..neg + pos {
            b.skip_ue();
            b.skip(1);
        }
        neg + pos
    };
    nd.push(count);
}

fn parse_sps(b: &mut Bits) -> Option<(usize, Sps)> {
    let mut s = Sps::default();
    b.skip(4);
    let max_sub = b.read(3) as usize;
    b.skip(1);
    b.skip(2);
    s.high_tier = b.bool();
    s.profile = b.read(5) as u32;
    b.skip(32 + 48);
    s.level = b.read(8) as u32;
    let flags: Vec<(bool, bool)> = (0..max_sub).map(|_| (b.bool(), b.bool())).collect();
    if max_sub > 0 {
        b.skip(2 * (8 - max_sub));
    }
    for (p, l) in flags {
        if p {
            b.skip(88);
        }
        if l {
            b.skip(8);
        }
    }
    let id = b.ue() as usize;
    s.chroma = b.ue();
    if s.chroma == 3 {
        b.skip(1);
    }
    b.skip_ue();
    b.skip_ue();
    if b.bool() {
        for _ in 0..4 {
            b.skip_ue();
        }
    }
    s.bit_depth = b.ue() + 8;
    b.skip_ue();
    let poc_bits = b.ue() as usize + 4;
    let present = b.bool();
    for _ in if present { 0 } else { max_sub }..=max_sub {
        for _ in 0..3 {
            b.skip_ue();
        }
    }
    for _ in 0..6 {
        b.skip_ue();
    }
    if b.bool() && b.bool() {
        for size in 0..4 {
            for m in (0..6).step_by(if size == 3 { 3 } else { 1 }) {
                let _ = m;
                if !b.bool() {
                    b.skip_ue();
                } else {
                    if size > 1 {
                        b.skip_ue();
                    }
                    for _ in 0..64.min(1 << (4 + (size << 1))) {
                        b.skip_ue();
                    }
                }
            }
        }
    }
    b.skip(2);
    if b.bool() {
        b.skip(8);
        b.skip_ue();
        b.skip_ue();
        b.skip(1);
    }
    let n = b.ue() as usize;
    if n > 64 {
        return None;
    }
    let mut nd = Vec::new();
    for i in 0..n {
        st_rps(b, i, n, &mut nd);
    }
    if b.bool() {
        for _ in 0..b.ue() {
            b.skip(poc_bits + 1);
        }
    }
    b.skip(2);
    if b.bool() && b.remain() > 0 {
        // VUI (up to timing info)
        if b.bool() && b.read(8) == 255 {
            b.skip(32);
        }
        if b.bool() {
            b.skip(1);
        }
        if b.bool() {
            b.skip(3);
            s.full_range = b.bool();
            if b.bool() {
                s.primaries = b.read(8) as u32;
                s.transfer = b.read(8) as u32;
                s.matrix = b.read(8) as u32;
            }
        }
        if b.bool() {
            b.skip_ue();
            b.skip_ue();
        }
        b.skip(3);
        if b.bool() {
            for _ in 0..4 {
                b.skip_ue();
            }
        }
        if b.bool() && b.remain() >= 64 {
            let (u, t) = (b.read(32) as u32, b.read(32) as u32);
            s.timing = Some((u, t));
        }
    }
    Some((id, s))
}

// The inner `if` reads bits while testing; moving it into a match guard would hide that side effect.
#[allow(clippy::collapsible_match)]
fn parse_sei(h: &mut Hevc, b: &mut Bits) {
    while b.remain() > 24 {
        let (mut t, mut sz) = (0u32, 0usize);
        loop {
            let v = b.byte() as u32;
            t += v;
            if v != 255 {
                break;
            }
        }
        loop {
            let v = b.byte() as usize;
            sz += v;
            if v != 255 {
                break;
            }
        }
        let end = b.pos + sz * 8;
        match t {
            137 if sz >= 24 => {
                for p in h.md_primaries.iter_mut() {
                    *p = b.read(16) as u16;
                }
                h.md_lum = (b.read(32) as u32, b.read(32) as u32);
                h.md_avail = true;
            }
            144 if sz >= 4 => {
                h.cll = b.read(16) as u16;
                h.fall = b.read(16) as u16;
                h.cll_avail = true;
            }
            4 if sz >= 6 => {
                if b.read(8) == 0xB5 && b.read(16) == 0x3C && b.read(16) == 1 && b.read(8) == 4 {
                    h.hdr10plus = true;
                }
            }
            _ => {}
        }
        b.pos = end;
    }
}

fn describe(s: &mut Stream, sp: &Sps, h: &Hevc) {
    let name = match sp.profile {
        1 => "Main",
        2 => "Main 10",
        3 => "Main Still Picture",
        4 => "Format Range Extensions",
        _ => "Unknown",
    };
    s.profile = Some(format!(
        "{} @ Level {}.{} @ {}",
        name,
        sp.level / 30,
        (sp.level % 30) / 3,
        if sp.high_tier { "High" } else { "Main" }
    ));
    let mut e = vec![
        match sp.chroma {
            0 => "4:0:0",
            1 => "4:2:0",
            2 => "4:2:2",
            _ => "4:4:4",
        }
        .to_string(),
        format!("{} bits", sp.bit_depth),
    ];
    let dv = s.pid >= 4117;
    if dv {
        e.push("Dolby Vision".into());
    } else {
        match sp.transfer {
            16 => e.push(if h.hdr10plus { "HDR10+" } else { "HDR10" }.into()),
            18 => e.push("HLG".into()),
            _ => {}
        }
    }
    // same wording and order as the Windows report
    e.push(if sp.full_range { "Full Range" } else { "Limited Range" }.into());
    if sp.primaries != 0 {
        e.push(
            match sp.primaries {
                1 => "BT.709",
                9 => "BT.2020",
                5 | 6 => "BT.601",
                _ => "Unspecified",
            }
            .into(),
        );
        e.push(
            match sp.transfer {
                1 | 6 => "BT.709",
                14 | 15 => "BT.2020",
                16 => "PQ",
                18 => "HLG",
                _ => "Unspecified",
            }
            .into(),
        );
        e.push(
            match sp.matrix {
                1 => "BT.709",
                5 | 6 => "BT.601",
                9 => "BT.2020 non-constant",
                10 => "BT.2020 constant",
                _ => "Unspecified",
            }
            .into(),
        );
    }
    if h.md_avail {
        let p = &h.md_primaries;
        let v = |i: usize| p[i] as f64 / 50000.0;
        // SEI order is G, B, R, white point
        let (r, g, bl) = ((v(4), v(5)), (v(0), v(1)), (v(2), v(3)));
        let near = |a: (f64, f64), x: f64, y: f64| (a.0 - x).abs() < 0.01 && (a.1 - y).abs() < 0.01;
        let name = if near(r, 0.680, 0.320) && near(g, 0.265, 0.690) && near(bl, 0.150, 0.060) {
            "Display P3".to_string()
        } else if near(r, 0.708, 0.292) && near(g, 0.170, 0.797) && near(bl, 0.131, 0.046) {
            "BT.2020".to_string()
        } else if near(r, 0.640, 0.330) && near(g, 0.300, 0.600) && near(bl, 0.150, 0.060) {
            "BT.709".to_string()
        } else {
            format!("R({:.4},{:.4}) G({:.4},{:.4}) B({:.4},{:.4})", r.0, r.1, g.0, g.1, bl.0, bl.1)
        };
        e.push(format!("Mastering display color primaries: {name}"));
        e.push(format!(
            "Mastering display luminance: min: {} cd/m2, max: {} cd/m2",
            h.md_lum.1 as f64 / 10000.0,
            h.md_lum.0 as f64 / 10000.0
        ));
    }
    // not in the original; shown only when the disc actually carries it
    if h.cll_avail && (h.cll > 0 || h.fall > 0) {
        e.push(format!("MaxCLL: {} cd/m2", h.cll));
        e.push(format!("MaxFALL: {} cd/m2", h.fall));
    }
    s.ext_info = e;
}

// The inner `if` reads bits while testing; moving it into a match guard would hide that side effect.
#[allow(clippy::collapsible_match)]
pub fn scan(s: &mut Stream, b: &mut Bits, tag: &mut Tag) {
    let raw = std::mem::take(&mut b.data);
    let mut h = *s.hevc.take().unwrap_or_default();
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= raw.len() {
        if raw[i] == 0 && raw[i + 1] == 0 && raw[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut last_sps = None;
    for (k, &st) in starts.iter().enumerate() {
        let en = starts.get(k + 1).map_or(raw.len(), |n| n - 3);
        if st + 2 > en {
            continue;
        }
        let nt = (raw[st] >> 1) & 0x3F;
        let mut nb = Bits::new(unescape(&raw[st + 2..en]));
        match nt {
            33 => {
                if let Some((id, sp)) = parse_sps(&mut nb) {
                    if id < 16 {
                        if h.sps.len() <= id {
                            h.sps.resize(id + 1, None);
                        }
                        h.sps[id] = Some(sp.clone());
                        last_sps = Some(sp);
                    }
                }
            }
            34 => {
                let id = nb.ue() as usize;
                nb.skip_ue();
                nb.skip(2);
                let extra = nb.read(3) as usize;
                if id < 64 {
                    if h.pps.len() <= id {
                        h.pps.resize(id + 1, None);
                    }
                    h.pps[id] = Some(Pps { extra_bits: extra });
                }
            }
            39 | 40 => parse_sei(&mut h, &mut nb),
            0..=9 | 16..=21 => {
                if nb.bool() {
                    if (16..=23).contains(&nt) {
                        nb.skip(1);
                    }
                    let pid = nb.ue() as usize;
                    let extra = h.pps.get(pid).and_then(|p| p.as_ref()).map_or(0, |p| p.extra_bits);
                    nb.skip(extra);
                    *tag = match nb.ue() {
                        2 => Some("I"),
                        1 => Some("P"),
                        0 => Some("B"),
                        _ => *tag,
                    };
                }
            }
            _ => {}
        }
    }
    if let Some(sp) = last_sps.or_else(|| h.sps.iter().flatten().next().cloned()) {
        if !s.initialized || h.md_avail || h.cll_avail || h.hdr10plus {
            describe(s, &sp, &h);
        }
        if s.fr_den == 0 {
            if let Some((u, t)) = sp.timing.filter(|&(u, _)| u > 0) {
                s.fr_num = t as i32;
                s.fr_den = u as i32;
            }
        }
        s.is_vbr = true;
        s.initialized = !(EXTENDED.load(Ordering::Relaxed) && sp.transfer == 16 && !h.md_avail);
    }
    s.hevc = Some(Box::new(h));
}
