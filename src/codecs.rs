//! Codec header scanners (ports of TSCodec*.cs). Each inspects the first PES payload(s)
//! of an elementary stream and fills in the stream's properties.

use crate::bits::{unescape, Bits};
use crate::stream::*;

pub type Tag = Option<&'static str>;

// ---------------------------------------------------------------- video

pub fn mpeg2(s: &mut Stream, b: &mut Bits, tag: &mut Tag) {
    let (mut parse, mut picture, mut seq_hdr, mut ext, mut seq_ext) = (0u32, 0, 0, 0, 0);
    for _ in 0..b.len() {
        parse = (parse << 8) | b.byte() as u32;
        if parse == 0x100 {
            picture = 2;
        } else if parse == 0x1B3 {
            seq_hdr = 7;
        } else if seq_hdr > 0 {
            seq_hdr -= 1;
            match seq_hdr {
                4 => s.height = (parse & 0xFFF) as i32,
                3 => {
                    s.aspect = ((parse & 0xF0) >> 4) as u8;
                    let (n, d) = match parse & 0xF {
                        1 => (24000, 1001),
                        2 => (24000, 1000),
                        3 => (25000, 1000),
                        4 => (30000, 1001),
                        5 => (30000, 1000),
                        6 => (50000, 1000),
                        7 => (60000, 1001),
                        8 => (60000, 1000),
                        _ => (0, 0),
                    };
                    s.fr_num = n;
                    s.fr_den = d;
                }
                0 => {
                    s.bitrate = (((parse & 0xFF_FFC0) >> 6) * 200) as i64;
                    s.is_vbr = true;
                    s.initialized = true;
                }
                _ => {}
            }
        } else if picture > 0 {
            picture -= 1;
            if picture == 0 {
                *tag = match (parse & 0x38) >> 3 {
                    1 => Some("I"),
                    2 => Some("P"),
                    3 => Some("B"),
                    _ => *tag,
                };
                if s.initialized {
                    return;
                }
            }
        } else if parse == 0x1B5 {
            ext = 1;
        } else if ext > 0 {
            ext -= 1;
            if ext == 0 && (parse & 0xF0) == 0x10 {
                seq_ext = 1;
            }
        } else if seq_ext > 0 {
            seq_ext -= 1;
            if seq_ext == 0 {
                s.interlaced = (parse & 0x8) >> 3 == 0;
            }
        }
    }
}

pub fn avc(s: &mut Stream, b: &mut Bits, tag: &mut Tag) {
    let data = if s.initialized { std::borrow::Cow::Borrowed(&b.data[..]) } else { std::borrow::Cow::Owned(unescape(&b.data)) };
    let (mut parse, mut aud, mut sps) = (0u32, 0, 0);
    let mut profile = "";
    let mut c3 = false;
    for &byte in data.iter() {
        parse = (parse << 8) | byte as u32;
        if parse == 0x109 {
            aud = 1;
        } else if aud > 0 {
            aud -= 1;
            if aud == 0 {
                *tag = match (parse & 0xFF) >> 5 {
                    0 | 3 | 5 => Some("I"),
                    1 | 4 | 6 => Some("P"),
                    2 | 7 => Some("B"),
                    _ => *tag,
                };
                if s.initialized {
                    return;
                }
            }
        } else if parse == 0x127 || parse == 0x167 {
            sps = 3;
        } else if sps > 0 {
            sps -= 1;
            if !s.initialized {
                match sps {
                    2 => {
                        profile = match parse & 0xFF {
                            66 => "Baseline Profile",
                            77 => "Main Profile",
                            88 => "Extended Profile",
                            100 => "High Profile",
                            110 => "High 10 Profile",
                            122 => "High 4:2:2 Profile",
                            144 => "High 4:4:4 Profile",
                            _ => "Unknown Profile",
                        }
                    }
                    1 => c3 = (parse & 0x10) != 0,
                    0 => {
                        let l = (parse & 0xFF) as u8;
                        let level = if l == 11 && c3 { "1b".to_string() } else { format!("{}.{}", l / 10, l % 10) };
                        s.profile = Some(format!("{} {}", profile, level));
                        s.is_vbr = true;
                        s.initialized = true;
                    }
                    _ => {}
                }
            }
        }
    }
}

pub fn mvc(s: &mut Stream) {
    s.is_vbr = true;
    s.initialized = true;
}

pub fn vc1(s: &mut Stream, b: &mut Bits, tag: &mut Tag) {
    let (mut parse, mut frame, mut seq) = (0u32, 0, 0);
    let mut interlaced = false;
    for _ in 0..b.len() {
        parse = (parse << 8) | b.byte() as u32;
        if parse == 0x10D {
            frame = 4;
        } else if frame > 0 {
            frame -= 1;
            if frame == 0 {
                let pt = if interlaced {
                    if parse & 0x8000_0000 == 0 { (parse & 0x7800_0000) >> 13 } else { (parse & 0x3C00_0000) >> 12 }
                } else {
                    (parse & 0xF000_0000) >> 14
                };
                *tag = if pt & 0x20000 == 0 {
                    Some("P")
                } else if pt & 0x10000 == 0 {
                    Some("B")
                } else if pt & 0x8000 == 0 {
                    Some("I")
                } else if pt & 0x4000 == 0 {
                    Some("BI")
                } else {
                    None
                };
                if s.initialized {
                    return;
                }
            }
        } else if parse == 0x10F {
            seq = 6;
        } else if seq > 0 {
            seq -= 1;
            match seq {
                5 => {
                    let level = (parse & 0x38) >> 3;
                    s.profile = Some(if (parse & 0xC0) >> 6 == 3 {
                        format!("Advanced Profile {}", level)
                    } else {
                        format!("Main Profile {}", level)
                    });
                }
                0 => interlaced = (parse & 0x40) >> 6 > 0,
                _ => {}
            }
            s.is_vbr = true;
            s.initialized = true;
        }
    }
}

// ---------------------------------------------------------------- AC-3 / E-AC-3

const AC3_BITRATE: [i64; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];
const AC3_CHANNELS: [i32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

fn ac3_chan_map(map: u32) -> i32 {
    (0..16).filter(|i| map & (1 << (15 - i)) != 0 && matches!(i, 5 | 6 | 9 | 10 | 11)).count() as i32 * 2
}

fn emdf_payload_config(b: &mut Bits) {
    let sample_offset = b.bool();
    if sample_offset {
        b.skip(12);
    }
    if b.bool() {
        b.skip(11);
    }
    if b.bool() {
        b.skip(2);
    }
    if b.bool() {
        b.skip(8);
    }
    if !b.bool() {
        b.skip(1);
        if !sample_offset && b.bool() {
            b.skip(9);
        }
    }
}

pub fn ac3(s: &mut Stream, b: &mut Bits) {
    if s.initialized {
        return;
    }
    match b.bytes(2) {
        Some(v) if v[0] == 0x0B && v[1] == 0x77 => {}
        _ => return,
    }
    let second_frame = s.channels > 0;
    let hdr = match b.bytes(4) {
        Some(h) => h,
        None => return,
    };
    let mut bsid = ((hdr[3] & 0xF8) >> 3) as u32;
    b.seek_bytes(-4);

    let (mut sr, mut frame_size, mut frame_size_code, mut num_blocks): (u32, u32, u32, u32);
    frame_size = 0u32;
    frame_size_code = 0u32;
    num_blocks = 0u32;
    let (channel_mode, mut lfe, dial_norm, dial_norm_ext);
    if bsid <= 10 {
        b.skip(16); // crc1
        sr = b.read(2) as u32;
        frame_size_code = b.read(6) as u32;
        bsid = b.read(5) as u32;
        b.skip(3);
        channel_mode = b.read(3) as u32;
        if channel_mode & 1 != 0 && channel_mode != 1 {
            b.skip(2);
        }
        if channel_mode & 4 != 0 {
            b.skip(2);
        }
        if channel_mode == 2 && b.read(2) == 2 {
            s.mode = AudioMode::Surround;
        }
        lfe = b.read(1) as u32;
        dial_norm = b.read(5) as u32;
        dial_norm_ext = 0;
        for skip in [8, 8, 7] {
            if b.bool() {
                b.skip(skip);
            }
        }
        if channel_mode == 0 {
            b.skip(5);
            for skip in [8, 8, 7] {
                if b.bool() {
                    b.skip(skip);
                }
            }
        }
        b.skip(2);
        if bsid == 6 {
            if b.bool() {
                b.skip(14);
            }
            if b.bool() {
                let dsurexmod = b.read(2);
                b.skip(2);
                b.skip(10);
                if dsurexmod == 2 {
                    s.mode = AudioMode::Extended;
                }
            }
        }
    } else {
        let frame_type = b.read(2);
        b.skip(3);
        frame_size = ((b.read(11) as u32) + 1) << 1;
        sr = b.read(2) as u32;
        if sr == 3 {
            sr = b.read(2) as u32;
            num_blocks = 3;
        } else {
            num_blocks = b.read(2) as u32;
        }
        channel_mode = b.read(3) as u32;
        lfe = b.read(1) as u32;
        bsid = b.read(5) as u32;
        dial_norm_ext = b.read(5) as u32;
        dial_norm = 0;
        if b.bool() {
            b.skip(8);
        }
        if channel_mode == 0 {
            b.skip(5);
            if b.bool() {
                b.skip(8);
            }
        }
        if frame_type == 1 {
            let mut core = s.clone();
            core.stype = AC3_AUDIO;
            s.core = Some(Box::new(core));
            if b.bool() {
                let chanmap = b.read(16) as u32;
                let core = s.core.as_ref().unwrap();
                s.channels = core.channels + ac3_chan_map(chanmap);
                lfe = core.lfe as u32;
            }
        }
        // search for the EMDF sync word bit by bit
        let mut emdf = false;
        loop {
            let start = b.pos;
            if b.read(16) == 0x5838 {
                emdf = true;
                break;
            }
            b.pos = start + 1;
            if b.bytepos() >= b.len() {
                break;
            }
        }
        if emdf {
            let container = b.read(16) as i64;
            let remain_after = b.remain() - container * 8;
            let mut version = b.read(2);
            if version == 3 {
                version += b.read(2);
            }
            if version > 0 {
                b.skip((b.remain() - remain_after).max(0) as usize);
            } else {
                if b.read(3) == 7 {
                    b.skip(2);
                }
                let mut id = b.read(5);
                if id > 0 && id < 16 {
                    if id == 0x1F {
                        b.skip(5);
                    }
                    emdf_payload_config(b);
                    let size = b.read(8) as usize * 8;
                    b.skip(size + 1);
                }
                loop {
                    id = b.read(5);
                    if id == 14 || b.bytepos() >= b.len() {
                        break;
                    }
                    if id == 0x1F {
                        b.skip(5);
                    }
                    emdf_payload_config(b);
                    let size = b.read(8) as usize * 8;
                    b.skip(size + 1);
                }
                if b.bytepos() < b.len() && id == 14 {
                    emdf_payload_config(b);
                    b.skip(12);
                    if b.read(6) > 0 {
                        s.has_ext = true;
                    }
                }
            }
        }
    }

    if channel_mode < 8 && s.channels == 0 {
        s.channels = AC3_CHANNELS[channel_mode as usize];
    }
    if s.mode == AudioMode::Unknown {
        s.mode = match channel_mode {
            0 => AudioMode::DualMono,
            2 => AudioMode::Stereo,
            _ => AudioMode::Unknown,
        };
    }
    s.sample_rate = match sr {
        0 => 48000,
        1 => 44100,
        2 => 32000,
        _ => 0,
    };
    if bsid <= 10 {
        let f = (frame_size_code >> 1) as usize;
        if f < 19 {
            s.bitrate = AC3_BITRATE[f] * 1000;
        }
    } else {
        s.bitrate = if num_blocks > 0 {
            (4.0 * frame_size as f64 * s.sample_rate as f64 / (num_blocks * 256) as f64) as i64
        } else {
            0
        };
        if let Some(c) = &s.core {
            s.bitrate += c.bitrate;
        }
    }
    s.lfe = lfe as i32;
    if s.stype != AC3_PLUS_SECONDARY {
        if (s.stype == AC3_PLUS_AUDIO && bsid == 6) || s.stype == AC3_AUDIO {
            s.dial_norm = -(dial_norm as i32);
        } else if s.stype == AC3_PLUS_AUDIO && second_frame {
            s.dial_norm = -(dial_norm_ext as i32);
        }
    }
    s.is_vbr = false;
    s.initialized = !(s.stype == AC3_PLUS_AUDIO && bsid == 6 && !second_frame);
}

// ---------------------------------------------------------------- Dolby TrueHD

pub fn truehd(s: &mut Stream, b: &mut Bits, tag: &mut Tag) {
    if s.initialized && s.core.as_ref().is_some_and(|c| c.initialized) {
        return;
    }
    let mut sync = 0u32;
    let mut found = false;
    for _ in 0..b.len() {
        sync = (sync << 8) | b.byte() as u32;
        if sync == 0xF872_6FBA {
            found = true;
            break;
        }
    }
    if !found {
        *tag = Some("CORE");
        let core = s.core.get_or_insert_with(|| Box::new(Stream { stype: AC3_AUDIO, ..Default::default() }));
        if !core.initialized {
            b.rewind();
            ac3(core, b);
        }
        return;
    }
    *tag = Some("HD");
    let ratebits = b.read(4) as i32;
    if ratebits != 0xF {
        s.sample_rate = (if ratebits & 8 != 0 { 44100 } else { 48000 }) << (ratebits & 7);
    }
    b.skip(15);
    s.channels = 0;
    s.lfe = 0;
    // (is_lfe, channel count) for each of the 13 channel-assignment flags
    for (lfe, ch) in [(1, 0), (0, 1), (0, 2), (0, 2), (0, 1), (0, 1), (0, 2), (0, 2), (0, 2), (0, 2), (1, 0), (0, 1), (0, 2)] {
        if b.bool() {
            s.lfe += lfe;
            s.channels += ch;
        }
    }
    b.skip(49);
    let peak = (b.read(15) * s.sample_rate as u64) >> 4;
    let peak_depth = peak as f64 / (s.channels + s.lfe).max(1) as f64 / s.sample_rate.max(1) as f64;
    s.bit_depth = if peak_depth > 14.0 { 24 } else { 16 };
    b.skip(79);
    let has_ext = b.bool();
    let num_ext = b.read(4) * 2 + 1;
    let mut has_content = b.read(4) != 0;
    if has_ext {
        for _ in 0..num_ext {
            if b.read(8) != 0 {
                has_content = true;
            }
        }
        if has_content {
            s.has_ext = true;
        }
    }
    s.is_vbr = true;
    if s.core.as_ref().is_some_and(|c| c.initialized) {
        s.initialized = true;
    }
}

// ---------------------------------------------------------------- LPCM

pub fn lpcm(s: &mut Stream, b: &mut Bits) {
    if s.initialized {
        return;
    }
    let h = match b.bytes(4) {
        Some(h) => h,
        None => return,
    };
    let flags = ((h[2] as u32) << 8) | h[3] as u32;
    let (ch, lfe) = match (flags & 0xF000) >> 12 {
        1 => (1, 0),
        3 => (2, 0),
        4 => (3, 0),
        5 => (3, 0),
        6 => (4, 0),
        7 => (4, 0),
        8 => (5, 0),
        9 => (5, 1),
        10 => (7, 0),
        11 => (7, 1),
        _ => (0, 0),
    };
    s.channels = ch;
    s.lfe = lfe;
    s.bit_depth = match (flags & 0xC0) >> 6 {
        1 => 16,
        2 => 20,
        3 => 24,
        _ => 0,
    };
    s.sample_rate = match (flags & 0xF00) >> 8 {
        1 => 48000,
        4 => 96000,
        5 => 192000,
        _ => 0,
    };
    s.bitrate = s.sample_rate as i64 * s.bit_depth as i64 * (s.channels + s.lfe) as i64;
    s.is_vbr = false;
    s.initialized = true;
}

// ---------------------------------------------------------------- DTS / DTS-HD

const DCA_SAMPLE_RATES: [i32; 16] = [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 96000, 192000];
const DCA_BIT_RATES: [i64; 32] = [
    32000, 56000, 64000, 96000, 112000, 128000, 192000, 224000, 256000, 320000, 384000, 448000, 512000, 576000, 640000,
    768000, 896000, 1024000, 1152000, 1280000, 1344000, 1408000, 1411200, 1472000, 1509000, 1920000, 2048000, 3072000,
    3840000, 1, 2, 3,
];
const DCA_BITS_PER_SAMPLE: [i32; 7] = [16, 16, 20, 20, 0, 24, 24];

pub fn dts(s: &mut Stream, b: &mut Bits, bitrate: i64) {
    if s.initialized {
        return;
    }
    let mut sync = 0u32;
    let mut found = false;
    for _ in 0..b.len() {
        sync = (sync << 8) | b.byte() as u32;
        if sync == 0x7FFE_8001 {
            found = true;
            break;
        }
    }
    if !found {
        return;
    }
    b.skip(6);
    let crc_present = b.read(1);
    b.skip(7);
    if b.read(14) < 95 {
        return;
    }
    b.skip(6);
    let sample_rate = b.read(4) as usize;
    let bit_rate = b.read(5) as usize;
    b.skip(8);
    let ext_coding = b.read(1);
    b.skip(1);
    let lfe = b.read(2);
    b.skip(1);
    if crc_present == 1 {
        b.skip(16);
    }
    b.skip(7);
    let pcm_res = b.read(3) as usize;
    b.skip(2);
    let dialog_norm = b.read(4) as i32;
    if pcm_res >= DCA_BITS_PER_SAMPLE.len() {
        return;
    }
    b.skip(4);
    let total = b.read(3) as i32 + 1 + ext_coding as i32;

    s.sample_rate = DCA_SAMPLE_RATES[sample_rate];
    s.channels = total;
    s.lfe = (lfe > 0) as i32;
    s.bit_depth = DCA_BITS_PER_SAMPLE[pcm_res];
    s.dial_norm = -dialog_norm;
    if pcm_res & 1 == 1 {
        s.mode = AudioMode::Extended;
    }
    s.bitrate = DCA_BIT_RATES[bit_rate];
    match s.bitrate {
        1 => {
            if bitrate > 0 {
                s.bitrate = bitrate;
                s.is_vbr = false;
                s.initialized = true;
            } else {
                s.bitrate = 0;
            }
        }
        2 | 3 => {
            s.is_vbr = true;
            s.initialized = true;
        }
        _ => {
            s.is_vbr = false;
            s.initialized = true;
        }
    }
}

const DTSHD_RATES: [i32; 16] = [
    0x1F40, 0x3E80, 0x7D00, 0xFA00, 0x1F400, 0x5622, 0xAC44, 0x15888, 0x2B110, 0x56220, 0x2EE0, 0x5DC0, 0xBB80, 0x17700,
    0x2EE00, 0x5DC00,
];

pub fn dtshd(s: &mut Stream, b: &mut Bits, bitrate: i64, tag: &mut Tag) {
    if s.initialized && (s.stype == DTS_HD_SECONDARY || s.core.as_ref().is_some_and(|c| c.initialized)) {
        return;
    }
    let mut sync = 0u32;
    let mut found = false;
    for _ in 0..b.len() {
        sync = (sync << 8) | b.byte() as u32;
        if sync == 0x6458_2025 {
            found = true;
            break;
        }
    }
    if !found {
        *tag = Some("CORE");
        let core = s.core.get_or_insert_with(|| Box::new(Stream { stype: DTS_AUDIO, ..Default::default() }));
        if !core.initialized {
            b.rewind();
            dts(core, b, bitrate);
        }
        return;
    }
    *tag = Some("HD");
    b.skip(8);
    let sub_stream_index = b.read(2) as usize;
    let blown_up = b.bool();
    b.skip(if blown_up { 32 } else { 24 });
    let mut num_assets = 1;
    let static_fields = b.bool();
    if static_fields {
        b.skip(5);
        if b.bool() {
            b.skip(36);
        }
        let num_audio = b.read(3) as usize + 1;
        num_assets = b.read(3) as usize + 1;
        for _ in 0..num_audio {
            b.read(sub_stream_index + 1);
        }
        for _ in 0..num_audio {
            for j in 0..=sub_stream_index {
                if (j + 1) % 2 == 1 {
                    b.skip(8);
                }
            }
        }
        if b.bool() {
            b.skip(2);
            let bits_mask = b.read(2) as usize * 4 + 4;
            let configs = b.read(2) + 1;
            for _ in 0..configs {
                b.skip(bits_mask);
            }
        }
    }
    for _ in 0..num_assets {
        b.skip(if blown_up { 20 } else { 16 });
    }
    for _ in 0..num_assets {
        b.skip(12);
        if static_fields {
            if b.bool() {
                b.skip(4);
            }
            if b.bool() {
                b.skip(24);
            }
            if b.bool() {
                let n = b.read(10) + 1;
                b.skip(n as usize * 8);
            }
            let bit_res = b.read(5) as i32 + 1;
            let max_rate = b.read(4) as usize;
            let total_ch = b.read(8) as i32 + 1;
            let mut mask = 0u64;
            if b.bool() {
                if total_ch > 2 {
                    b.skip(1);
                }
                if total_ch > 6 {
                    b.skip(1);
                }
                if b.bool() {
                    let bits = b.read(2) as usize * 4 + 4;
                    mask = b.read(bits);
                }
            }
            s.sample_rate = DTSHD_RATES[max_rate];
            s.bit_depth = bit_res;
            s.lfe = 0;
            if mask & 0x8 != 0 {
                s.lfe += 1;
            }
            if mask & 0x1000 != 0 {
                s.lfe += 1;
            }
            s.channels = total_ch - s.lfe;
        }
        if num_assets > 1 {
            break;
        }
    }
    // look for extension substreams and the DTS:X marker
    let mut w = 0u32;
    while b.bytepos() < b.len() {
        w = (w << 8) | b.byte() as u32;
        if matches!(w, 0x41A2_9547 | 0x655E_315E | 0x0A80_1921 | 0x1D95_F262 | 0x4700_4A03 | 0x5A5A_5A5A) {
            let mut w3 = 0u32;
            for _ in b.bytepos()..b.len() {
                w3 = (w3 << 8) | b.byte() as u32;
                if w3 == 0x0200_0850 {
                    s.has_ext = true;
                    break;
                }
            }
        }
        if s.has_ext {
            break;
        }
    }
    if let Some(core) = &s.core {
        if core.mode == AudioMode::Extended && s.channels == 5 {
            s.mode = AudioMode::Extended;
        }
    }
    if s.stype == DTS_HD_MASTER_AUDIO {
        s.is_vbr = true;
        s.initialized = true;
    } else if bitrate > 0 {
        s.is_vbr = false;
        s.bitrate = bitrate;
        if let Some(c) = &s.core {
            s.bitrate += c.bitrate;
        }
        s.initialized = s.bitrate > 0;
    }
}

// ---------------------------------------------------------------- MPEG audio / AAC

const MPA_BITRATE: [[[i32; 16]; 4]; 4] = [
    // MPEG 2.5
    [
        [0; 16],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0],
    ],
    [[0; 16]; 4], // reserved
    // MPEG 2
    [
        [0; 16],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0],
    ],
    // MPEG 1
    [
        [0; 16],
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0],
        [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0],
    ],
];
const MPA_SAMPLE_RATE: [[i32; 4]; 4] =
    [[11025, 12000, 8000, 0], [0; 4], [22050, 24000, 16000, 0], [44100, 48000, 32000, 0]];
const MPA_VERSION: [&str; 4] = ["MPEG 2.5", "Unknown MPEG", "MPEG 2", "MPEG 1"];
const MPA_LAYER: [&str; 4] = ["Unknown Layer", "Layer III", "Layer II", "Layer I"];

pub fn mpa(s: &mut Stream, b: &mut Bits) {
    if s.initialized {
        return;
    }
    if b.read(11) << 5 != 0xFFE0 {
        return;
    }
    let version = b.read(2) as usize;
    let layer = b.read(2) as usize;
    b.skip(1);
    let br_index = b.read(4) as usize;
    let sr_index = b.read(2) as usize;
    b.skip(2);
    let channel_mode = b.read(2) as usize;
    s.bitrate = MPA_BITRATE[version][layer][br_index] as i64 * 1000;
    s.sample_rate = MPA_SAMPLE_RATE[version][sr_index];
    s.mode = [AudioMode::Stereo, AudioMode::JointStereo, AudioMode::DualMono, AudioMode::Mono][channel_mode];
    s.channels = [2, 2, 2, 1][channel_mode];
    s.lfe = 0;
    s.codec_text = format!("{} {}", MPA_VERSION[version], MPA_LAYER[layer]);
    s.is_vbr = false;
    s.initialized = true;
}

const AAC_RATES: [i32; 14] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350, 0];

pub fn aac(s: &mut Stream, b: &mut Bits) {
    if s.initialized {
        return;
    }
    if b.read(12) != 0xFFF {
        return;
    }
    let id = b.read(1) as usize;
    b.skip(2);
    b.skip(1);
    let profile = b.read(2);
    let sr_index = b.read(4) as usize;
    b.skip(1);
    let channel_mode = b.read(3) as usize;
    s.sample_rate = AAC_RATES.get(sr_index).copied().unwrap_or(0);
    s.mode = [
        AudioMode::Unknown,
        AudioMode::Mono,
        AudioMode::Stereo,
        AudioMode::Extended,
        AudioMode::Surround,
        AudioMode::Surround,
        AudioMode::Surround,
        AudioMode::Surround,
    ][channel_mode];
    s.channels = [0, 1, 2, 3, 4, 5, 6, 8][channel_mode];
    if channel_mode >= 7 {
        s.channels -= 1;
        s.lfe = 1;
    } else {
        s.lfe = 0;
    }
    let profile = match profile {
        0 => "AAC Main",
        1 => "AAC LC",
        2 => "AAC SSR",
        3 => "AAC LTP",
        _ => "",
    };
    s.codec_text = format!("{} {}", ["MPEG-4", "MPEG-2"][id], profile);
    s.is_vbr = true;
    s.initialized = true;
}

// ---------------------------------------------------------------- Presentation Graphics

pub fn pgs(s: &mut Stream, b: &mut Bits, tag: &mut Tag) {
    match b.byte() {
        0x15 => {
            // ODS
            b.read(16);
            b.read(16);
            if !s.last_frame.finished {
                if s.last_frame.forced {
                    s.forced_captions += 1;
                    *tag = Some("F");
                } else {
                    s.captions += 1;
                    *tag = Some("N");
                }
            }
        }
        0x16 => {
            // PCS
            b.read(16);
            if !s.initialized {
                s.width = b.read(16) as i32;
                s.height = b.read(16) as i32;
                s.initialized = true;
            } else {
                b.read(32);
            }
            b.byte();
            let comp = b.read(16) as u32;
            b.byte();
            b.read(16);
            let objects = b.byte();
            for _ in 0..objects {
                b.read(16);
                b.byte();
                let forced = b.byte();
                b.skip(6 * 16);
                s.last_frame = Frame { forced: forced & 0x40 != 0, finished: false };
                s.caption_ids.insert(comp);
            }
        }
        0x80 => {
            s.last_frame.finished = true;
        }
        _ => {}
    }
    s.is_vbr = true;
}
