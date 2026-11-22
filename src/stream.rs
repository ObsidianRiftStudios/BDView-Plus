//! Elementary stream model (port of TSStream / TSVideoStream / TSAudioStream / ...).

use crate::lang::language_name;
use std::collections::{HashMap, HashSet};

pub const MPEG1_VIDEO: u8 = 0x01;
pub const MPEG2_VIDEO: u8 = 0x02;
pub const MPEG1_AUDIO: u8 = 0x03;
pub const MPEG2_AUDIO: u8 = 0x04;
pub const MPEG2_AAC: u8 = 0x0F;
pub const MPEG4_AAC: u8 = 0x11;
pub const AVC_VIDEO: u8 = 0x1B;
pub const MVC_VIDEO: u8 = 0x20;
pub const HEVC_VIDEO: u8 = 0x24;
pub const LPCM_AUDIO: u8 = 0x80;
pub const AC3_AUDIO: u8 = 0x81;
pub const DTS_AUDIO: u8 = 0x82;
pub const TRUEHD_AUDIO: u8 = 0x83;
pub const AC3_PLUS_AUDIO: u8 = 0x84;
pub const DTS_HD_AUDIO: u8 = 0x85;
pub const DTS_HD_MASTER_AUDIO: u8 = 0x86;
pub const PGS: u8 = 0x90;
pub const IGS: u8 = 0x91;
pub const SUBTITLE: u8 = 0x92;
pub const AC3_PLUS_SECONDARY: u8 = 0xA1;
pub const DTS_HD_SECONDARY: u8 = 0xA2;
pub const VC1_VIDEO: u8 = 0xEA;

/// C# Math.Round semantics (banker's rounding).
pub fn rnd(x: f64) -> f64 {
    x.round_ties_even()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AudioMode {
    #[default]
    Unknown,
    DualMono,
    Stereo,
    Surround,
    Extended,
    JointStereo,
    Mono,
}

#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub forced: bool,
    pub finished: bool,
}

#[derive(Clone, Default)]
pub struct Stream {
    pub pid: u16,
    pub stype: u8,
    pub lang: String,
    pub lang_name: String,
    pub bitrate: i64,
    pub active_bitrate: i64,
    pub is_vbr: bool,
    pub initialized: bool,
    pub hidden: bool,
    pub payload_bytes: u64,
    pub packet_count: u64,
    pub packet_seconds: f64,
    // video
    pub height: i32,
    pub interlaced: bool,
    pub fr_num: i32,
    pub fr_den: i32,
    pub aspect: u8,
    pub profile: Option<String>,
    pub ext_info: Vec<String>,
    pub hevc: Option<Box<crate::hevc::Hevc>>,
    // audio
    pub layout: u8,
    pub sample_rate: i32,
    pub channels: i32,
    pub bit_depth: i32,
    pub lfe: i32,
    pub dial_norm: i32,
    pub has_ext: bool,
    pub mode: AudioMode,
    pub core: Option<Box<Stream>>,
    pub codec_text: String,
    // graphics
    pub width: i32,
    pub captions: i32,
    pub forced_captions: i32,
    pub last_frame: Frame,
    pub caption_ids: HashSet<u32>,
}

impl Stream {
    pub fn new(pid: u16, stype: u8) -> Self {
        let mut s = Stream { pid, stype, ..Default::default() };
        if s.is_graphics() {
            s.is_vbr = true;
        }
        if s.is_text() {
            s.is_vbr = true;
            s.initialized = true;
        }
        s
    }
    pub fn set_lang(&mut self, code: &str) {
        self.lang = code.to_string();
        self.lang_name = language_name(code);
    }
    pub fn is_video(&self) -> bool {
        matches!(self.stype, 0x01 | 0x02 | 0x1B | 0x20 | 0x24 | 0xEA)
    }
    pub fn is_audio(&self) -> bool {
        matches!(self.stype, 0x03 | 0x04 | 0x0F | 0x11 | 0x80..=0x86 | 0xA1 | 0xA2)
    }
    pub fn is_graphics(&self) -> bool {
        matches!(self.stype, PGS | IGS)
    }
    pub fn is_text(&self) -> bool {
        self.stype == SUBTITLE
    }

    pub fn set_video_format(&mut self, f: u8) {
        let (h, i) = match f {
            1 => (480, true),
            2 => (576, true),
            3 => (480, false),
            4 => (1080, true),
            5 => (720, false),
            6 => (1080, false),
            7 => (576, false),
            8 => (2160, false),
            _ => return,
        };
        self.height = h;
        self.interlaced = i;
    }
    pub fn set_frame_rate(&mut self, r: u8) {
        let (n, d) = match r {
            1 => (24000, 1001),
            2 => (24000, 1000),
            3 => (25000, 1000),
            4 => (30000, 1001),
            6 => (50000, 1000),
            7 => (60000, 1001),
            _ => return,
        };
        self.fr_num = n;
        self.fr_den = d;
    }
    pub fn convert_sample_rate(r: u8) -> i32 {
        match r {
            1 => 48000,
            4 | 14 => 96000,
            5 | 12 => 192000,
            _ => 0,
        }
    }

    pub fn codec_name(&self) -> String {
        let ext = self.has_ext;
        let extended = self.mode == AudioMode::Extended;
        match self.stype {
            MPEG1_VIDEO => "MPEG-1 Video",
            MPEG2_VIDEO => "MPEG-2 Video",
            AVC_VIDEO => "MPEG-4 AVC Video",
            MVC_VIDEO => "MPEG-4 MVC Video",
            HEVC_VIDEO => "MPEG-H HEVC Video",
            VC1_VIDEO => "VC-1 Video",
            MPEG1_AUDIO | MPEG2_AUDIO | MPEG2_AAC | MPEG4_AAC => return self.codec_text.clone(),
            LPCM_AUDIO => "LPCM Audio",
            AC3_AUDIO if extended => "Dolby Digital EX Audio",
            AC3_AUDIO => "Dolby Digital Audio",
            AC3_PLUS_AUDIO if ext => "Dolby Digital Plus/Atmos Audio",
            AC3_PLUS_AUDIO | AC3_PLUS_SECONDARY => "Dolby Digital Plus Audio",
            TRUEHD_AUDIO if ext => "Dolby TrueHD/Atmos Audio",
            TRUEHD_AUDIO => "Dolby TrueHD Audio",
            DTS_AUDIO if extended => "DTS-ES Audio",
            DTS_AUDIO => "DTS Audio",
            DTS_HD_AUDIO if ext => "DTS:X High-Res Audio",
            DTS_HD_AUDIO => "DTS-HD High-Res Audio",
            DTS_HD_SECONDARY => "DTS Express",
            DTS_HD_MASTER_AUDIO if ext => "DTS:X Master Audio",
            DTS_HD_MASTER_AUDIO => "DTS-HD Master Audio",
            PGS => "Presentation Graphics",
            IGS => "Interactive Graphics",
            SUBTITLE => "Subtitle",
            _ => "UNKNOWN",
        }
        .to_string()
    }

    pub fn codec_alt_name(&self) -> String {
        let ext = self.has_ext;
        match self.stype {
            MPEG1_VIDEO => "MPEG-1",
            MPEG2_VIDEO => "MPEG-2",
            AVC_VIDEO => "AVC",
            MVC_VIDEO => "MVC",
            HEVC_VIDEO => "HEVC",
            VC1_VIDEO => "VC-1",
            MPEG1_AUDIO => "MP1",
            MPEG2_AUDIO => "MP2",
            MPEG2_AAC => "MPEG-2 AAC",
            MPEG4_AAC => "MPEG-4 AAC",
            LPCM_AUDIO => "LPCM",
            AC3_AUDIO => "DD AC3",
            AC3_PLUS_AUDIO | AC3_PLUS_SECONDARY => "DD AC3+",
            TRUEHD_AUDIO if ext => "Dolby Atmos",
            TRUEHD_AUDIO => "Dolby TrueHD",
            DTS_AUDIO => "DTS",
            DTS_HD_AUDIO if ext => "DTS:X Hi-Res",
            DTS_HD_AUDIO => "DTS-HD Hi-Res",
            DTS_HD_SECONDARY => "DTS Express",
            DTS_HD_MASTER_AUDIO if ext => "DTS:X Master",
            DTS_HD_MASTER_AUDIO => "DTS-HD Master",
            PGS => "PGS",
            IGS => "IGS",
            SUBTITLE => "SUB",
            _ => "UNKNOWN",
        }
        .to_string()
    }

    pub fn codec_short_name(&self) -> String {
        let ext = self.has_ext;
        let extended = self.mode == AudioMode::Extended;
        match self.stype {
            AC3_AUDIO if extended => "AC3-EX".into(),
            AC3_AUDIO => "AC3".into(),
            AC3_PLUS_AUDIO | AC3_PLUS_SECONDARY => "AC3+".into(),
            TRUEHD_AUDIO if ext => "Atmos".into(),
            TRUEHD_AUDIO => "TrueHD".into(),
            DTS_AUDIO if extended => "DTS-ES".into(),
            DTS_AUDIO => "DTS".into(),
            DTS_HD_AUDIO if ext => "DTS:X HR".into(),
            DTS_HD_AUDIO => "DTS-HD HR".into(),
            DTS_HD_MASTER_AUDIO if ext => "DTS:X MA".into(),
            DTS_HD_MASTER_AUDIO => "DTS-HD MA".into(),
            _ => self.codec_alt_name(),
        }
    }

    pub fn channel_description(&self) -> String {
        let mut d = String::new();
        if self.channels > 0 {
            d += &format!("{}.{}", self.channels, self.lfe);
        } else {
            d += match self.layout {
                1 => "1.0",
                3 => "2.0",
                6 => "5.1",
                _ => "",
            };
        }
        if self.mode == AudioMode::Extended {
            if self.stype == AC3_AUDIO {
                d += "-EX";
            }
            if matches!(self.stype, DTS_AUDIO | DTS_HD_AUDIO | DTS_HD_MASTER_AUDIO) {
                d += "-ES";
            }
        }
        d
    }

    pub fn description(&self) -> String {
        if self.is_video() {
            self.video_description()
        } else if self.is_audio() {
            self.audio_description()
        } else if self.is_graphics() {
            self.graphics_description()
        } else {
            String::new()
        }
    }

    fn video_description(&self) -> String {
        let mut d = String::new();
        if self.height > 0 {
            d += &format!("{}{} / ", self.height, if self.interlaced { "i" } else { "p" });
        }
        if self.fr_num > 0 && self.fr_den > 0 {
            if self.fr_num % self.fr_den == 0 {
                d += &format!("{} fps / ", self.fr_num / self.fr_den);
            } else {
                d += &format!("{:.3} fps / ", self.fr_num as f64 / self.fr_den as f64);
            }
        }
        match self.aspect {
            2 => d += "4:3 / ",
            3 => d += "16:9 / ",
            _ => {}
        }
        if let Some(p) = &self.profile {
            d += &format!("{} / ", p);
        }
        if self.stype == HEVC_VIDEO && !self.ext_info.is_empty() {
            d += &self.ext_info.join(" / ");
        }
        d.strip_suffix(" / ").map(str::to_string).unwrap_or(d)
    }

    fn audio_description(&self) -> String {
        let mut d = self.channel_description();
        if self.sample_rate > 0 {
            d += &format!(" / {} kHz", self.sample_rate / 1000);
        }
        if self.bitrate > 0 {
            let core_br = match (&self.core, self.stype) {
                (Some(c), TRUEHD_AUDIO) => c.bitrate,
                _ => 0,
            };
            d += &format!(" / {:5} kbps", rnd((self.bitrate - core_br) as f64 / 1000.0) as u32);
        }
        if self.bit_depth > 0 {
            d += &format!(" / {}-bit", self.bit_depth);
        }
        if self.dial_norm != 0 {
            d += &format!(" / DN {}dB", self.dial_norm);
        }
        if self.channels == 2 {
            match self.mode {
                AudioMode::DualMono => d += " / Dual Mono",
                AudioMode::Surround => d += " / Dolby Surround",
                AudioMode::JointStereo => d += " / Joint Stereo",
                _ => {}
            }
        }
        if let Some(c) = &self.core {
            let codec = match c.stype {
                AC3_AUDIO => "AC3 Embedded",
                DTS_AUDIO => "DTS Core",
                AC3_PLUS_AUDIO => "DD+ Embedded",
                _ => "",
            };
            d += &format!(" ({}: {})", codec, c.description());
        }
        d
    }

    fn graphics_description(&self) -> String {
        let mut d = String::new();
        if self.width > 0 || self.height > 0 {
            d = format!("{}x{}", self.width, self.height);
        }
        if self.captions > 0 || self.forced_captions > 0 {
            if self.captions > 0 {
                d += &format!(" / {} Caption{}", self.captions, if self.captions > 1 { "s" } else { "" });
            }
            if self.forced_captions > 0 {
                let s = if self.forced_captions > 1 { "s" } else { "" };
                if self.captions > 0 {
                    d += &format!(" ( + {} Forced Caption{})", self.forced_captions, s);
                } else {
                    d += &format!(" / {} Forced Caption{}", self.forced_captions, s);
                }
            }
        }
        d
    }
}

/// Insertion-ordered stream collection keyed by PID.
#[derive(Default, Clone)]
pub struct StreamSet {
    pub list: Vec<Stream>,
    idx: HashMap<u16, usize>,
}

impl StreamSet {
    pub fn get(&self, pid: u16) -> Option<&Stream> {
        self.idx.get(&pid).map(|&i| &self.list[i])
    }
    pub fn get_mut(&mut self, pid: u16) -> Option<&mut Stream> {
        match self.idx.get(&pid) {
            Some(&i) => Some(&mut self.list[i]),
            None => None,
        }
    }
    pub fn contains(&self, pid: u16) -> bool {
        self.idx.contains_key(&pid)
    }
    pub fn insert(&mut self, s: Stream) {
        match self.idx.get(&s.pid) {
            Some(&i) => self.list[i] = s,
            None => {
                self.idx.insert(s.pid, self.list.len());
                self.list.push(s);
            }
        }
    }
    pub fn clear(&mut self) {
        self.list.clear();
        self.idx.clear();
    }
}
