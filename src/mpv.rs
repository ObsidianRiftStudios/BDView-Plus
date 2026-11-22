//! Video preview through libmpv, loaded at run time (nothing is linked, so the executable stays small).
//! mpv renders into an OpenGL texture that the GUI blits into its window; playlists are read through the
//! libbluray handle the scan already holds (see `cb`), so MakeMKV's libmmbd decrypts them exactly as it does for the scan.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
}

unsafe fn lib(names: &[&str]) -> *mut c_void {
    for n in names {
        let c = CString::new(*n).unwrap();
        let h = dlopen(c.as_ptr(), 2);
        if !h.is_null() {
            return h;
        }
    }
    null_mut()
}

unsafe fn sym<T: Copy>(h: *mut c_void, name: &str) -> Result<T, String> {
    let c = CString::new(name).unwrap();
    let p = dlsym(h, c.as_ptr());
    if p.is_null() {
        return Err(format!("libmpv is missing {name}"));
    }
    Ok(std::mem::transmute_copy::<*mut c_void, T>(&p))
}

#[repr(C)]
struct Param {
    ty: c_int,
    data: *mut c_void,
}
#[repr(C)]
struct InitParams {
    get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    ctx: *mut c_void,
}
#[repr(C)]
struct FboParam {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}
#[repr(C)]
struct Event {
    id: c_int,
    error: c_int,
    reply: u64,
    data: *mut c_void,
}
#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}
#[repr(C)]
struct LogMsg {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
}

const P_API: c_int = 1;
const P_INIT: c_int = 2;
const P_FBO: c_int = 3;
const P_FLIP: c_int = 4;
const FMT_FLAG: c_int = 3;
const FMT_DOUBLE: c_int = 5;
const EV_LOG: c_int = 2;
const EV_END: c_int = 7;
const EV_LOADED: c_int = 8;

struct Api {
    create: unsafe extern "C" fn() -> *mut c_void,
    init: unsafe extern "C" fn(*mut c_void) -> c_int,
    set_opt: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    set_prop_str: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    get_prop_str: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    get_prop: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    command_async: unsafe extern "C" fn(*mut c_void, u64, *const *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *mut Event,
    log_level: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
    free: unsafe extern "C" fn(*mut c_void),
    err: unsafe extern "C" fn(c_int) -> *const c_char,
    rc_create: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut Param) -> c_int,
    rc_render: unsafe extern "C" fn(*mut c_void, *mut Param) -> c_int,
    rc_update: unsafe extern "C" fn(*mut c_void) -> u64,
    #[cfg(feature = "bluray")]
    add_ro: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, OpenRo) -> c_int,
}

fn load() -> Result<Api, String> {
    unsafe {
        let h = lib(&["libmpv.so.2", "libmpv.so.1", "libmpv.so"]);
        if h.is_null() {
            return Err("libmpv was not found. Install it (Debian/Ubuntu: sudo apt install libmpv2) to use the preview.".into());
        }
        Ok(Api {
            create: sym(h, "mpv_create")?,
            init: sym(h, "mpv_initialize")?,
            set_opt: sym(h, "mpv_set_option_string")?,
            set_prop_str: sym(h, "mpv_set_property_string")?,
            get_prop_str: sym(h, "mpv_get_property_string")?,
            get_prop: sym(h, "mpv_get_property")?,
            command_async: sym(h, "mpv_command_async")?,
            wait_event: sym(h, "mpv_wait_event")?,
            log_level: sym(h, "mpv_request_log_messages")?,
            free: sym(h, "mpv_free")?,
            err: sym(h, "mpv_error_string")?,
            rc_create: sym(h, "mpv_render_context_create")?,
            rc_render: sym(h, "mpv_render_context_render")?,
            rc_update: sym(h, "mpv_render_context_update")?,
            #[cfg(feature = "bluray")]
            add_ro: sym(h, "mpv_stream_cb_add_ro")?,
        })
    }
}

// OpenGL function lookup for mpv: whichever of EGL or GLX the window uses.
static PROC_FN: AtomicUsize = AtomicUsize::new(0);
static PROC_EGL: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn get_proc(_: *mut c_void, name: *const c_char) -> *mut c_void {
    let f = PROC_FN.load(Ordering::Relaxed);
    if f == 0 {
        return null_mut();
    }
    let f: unsafe extern "C" fn(*const c_char) -> *mut c_void = std::mem::transmute(f);
    f(name)
}

unsafe fn find_proc() -> bool {
    let egl = lib(&["libEGL.so.1"]);
    if !egl.is_null() {
        if let (Ok(cur), Ok(gp)) = (
            sym::<unsafe extern "C" fn() -> *mut c_void>(egl, "eglGetCurrentContext"),
            sym::<usize>(egl, "eglGetProcAddress"),
        ) {
            if !cur().is_null() {
                PROC_FN.store(gp, Ordering::Relaxed);
                PROC_EGL.store(true, Ordering::Relaxed);
                return true;
            }
        }
    }
    let gl = lib(&["libGL.so.1", "libGL.so"]);
    if !gl.is_null() {
        for n in ["glXGetProcAddressARB", "glXGetProcAddress"] {
            if let Ok(gp) = sym::<usize>(gl, n) {
                PROC_FN.store(gp, Ordering::Relaxed);
                return true;
            }
        }
    }
    false
}

/// Disc playlists are not opened by mpv's own `bd://` support (it would open the disc a second time, start a
/// second decryption session and read every playlist on the disc). The preview registers a `tuxbd://` stream
/// that reads the playlist through the libbluray handle the scan already holds.
#[cfg(feature = "bluray")]
mod cb {
    use super::*;
    use crate::bluray::{Disc, Stream};
    use crate::disc::Seg;
    use std::sync::{Arc, Mutex};

    pub static NEXT: Mutex<Option<(Arc<Disc>, Vec<Seg>)>> = Mutex::new(None);
    pub static ERR: Mutex<String> = Mutex::new(String::new());

    #[repr(C)]
    pub struct Info {
        cookie: *mut c_void,
        read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, u64) -> i64>,
        seek: Option<unsafe extern "C" fn(*mut c_void, i64) -> i64>,
        size: Option<unsafe extern "C" fn(*mut c_void) -> i64>,
        close: Option<unsafe extern "C" fn(*mut c_void)>,
    }

    pub unsafe extern "C" fn open(_: *mut c_void, _uri: *mut c_char, info: *mut Info) -> c_int {
        let Some((d, segs)) = NEXT.lock().unwrap().take() else { return -13 };
        let mut s = d.stream(segs);
        if s.seek(0) < 0 {
            *ERR.lock().unwrap() = "libbluray could not read the stream file (is AACS decryption set up?)".into();
            return -13;
        }
        let i = &mut *info;
        i.cookie = Box::into_raw(Box::new(s)) as *mut c_void;
        i.read = Some(read);
        i.seek = Some(seek);
        i.size = Some(size);
        i.close = Some(close);
        0
    }
    unsafe extern "C" fn read(c: *mut c_void, buf: *mut c_char, n: u64) -> i64 {
        (*(c as *mut Stream)).read(std::slice::from_raw_parts_mut(buf as *mut u8, n as usize))
    }
    unsafe extern "C" fn seek(c: *mut c_void, off: i64) -> i64 {
        let r = (*(c as *mut Stream)).seek(off);
        if r < 0 {
            -20
        } else {
            r
        }
    }
    unsafe extern "C" fn size(c: *mut c_void) -> i64 {
        (*(c as *mut Stream)).size()
    }
    unsafe extern "C" fn close(c: *mut c_void) {
        drop(Box::from_raw(c as *mut Stream));
    }
}

#[cfg(feature = "bluray")]
type OpenRo = unsafe extern "C" fn(*mut c_void, *mut c_char, *mut cb::Info) -> c_int;

/// Hardware decoders to try, by the GPUs the system reports (PCI vendor ids under /sys/class/drm).
/// AMD and Intel decode through VA-API, NVIDIA through NVDEC; the "-copy" forms are the fallback when
/// the zero-copy route cannot be used with the window's OpenGL context. Override with TUXINFO_HWDEC.
fn hwdec_list() -> String {
    if let Ok(v) = std::env::var("TUXINFO_HWDEC") {
        return v;
    }
    let mut list: Vec<&str> = Vec::new();
    let mut seen = Vec::new();
    for e in std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !n.starts_with("card") || n.contains('-') {
            continue;
        }
        let Ok(v) = std::fs::read_to_string(e.path().join("device/vendor")) else {
            continue;
        };
        let v = v.trim().trim_start_matches("0x").to_ascii_lowercase();
        if seen.contains(&v) {
            continue;
        }
        match v.as_str() {
            "10de" => list.extend(["nvdec", "nvdec-copy"]),
            "1002" | "8086" => list.extend(["vaapi", "vaapi-copy"]),
            _ => continue,
        }
        seen.push(v);
    }
    if list.is_empty() {
        "auto-safe".into()
    } else {
        list.join(",")
    }
}

/// What to play.
pub enum Target {
    /// A plain file (a .m2ts picked by the user).
    File(String),
    /// A playlist of a disc, folder or image; `start`/`length` (seconds) narrow it to one stream file.
    Playlist {
        device: String,
        name: String,
        start: f64,
        length: Option<f64>,
        /// Byte ranges of the clips, and the open disc to read them from (otherwise mpv's own `bd://` is used).
        #[cfg(feature = "bluray")]
        segs: Vec<crate::disc::Seg>,
        #[cfg(feature = "bluray")]
        bd: Option<std::sync::Arc<crate::bluray::Disc>>,
    },
}

#[derive(Clone, Default)]
pub struct Track {
    pub kind: String,
    pub id: i64,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, Default)]
pub struct Status {
    pub pos: f64,
    pub dur: f64,
    pub paused: bool,
    pub volume: f64,
    pub playing: bool,
    pub tracks: Vec<Track>,
    /// Last problem reported by mpv or libbluray.
    pub msg: String,
    /// Hardware decoder in use (mpv's hwdec-current): "vaapi", "nvdec-copy", ... or "no" for software.
    pub hw: String,
}

pub struct Player {
    api: Api,
    h: *mut c_void,
    rc: *mut c_void,
    rc_failed: bool,
    /// Waits here until the render context exists (mpv's video output needs it before a file can load).
    pending: Option<Target>,
    fbo: Option<(eframe::glow::Framebuffer, eframe::glow::Texture, i32, i32)>,
    pub st: Status,
}

// Only ever used from the UI thread; the Arc<Mutex<..>> in the GUI needs these to hand it to the paint callback.
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

impl Player {
    pub fn new() -> Result<Player, String> {
        let api = load()?;
        // libmpv prefers libaacs; point libbluray at MakeMKV's libmmbd the same way the scan does.
        #[cfg(feature = "bluray")]
        crate::bluray::prefer_mmbd();
        unsafe {
            let h = (api.create)();
            if h.is_null() {
                return Err("mpv could not be created".into());
            }
            let set = |k: &str, v: &str| {
                let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
                (api.set_opt)(h, k.as_ptr(), v.as_ptr());
            };
            set("vo", "libmpv");
            set("hwdec", &hwdec_list());
            set("keep-open", "yes");
            set("osc", "no");
            set("input-default-bindings", "no");
            set("terminal", "no");
            set("config", "no");
            set("audio-display", "no");
            set("idle", "yes");
            let r = (api.init)(h);
            if r < 0 {
                return Err(format!("mpv failed to start: {}", cstr((api.err)(r))));
            }
            #[cfg(feature = "bluray")]
            (api.add_ro)(h, c"tuxbd".as_ptr(), null_mut(), cb::open);
            let lv = CString::new("warn").unwrap();
            (api.log_level)(h, lv.as_ptr());
            Ok(Player {
                api,
                h,
                rc: null_mut(),
                rc_failed: false,
                pending: None,
                fbo: None,
                st: Status {
                    volume: 100.0,
                    ..Default::default()
                },
            })
        }
    }

    fn set(&self, k: &str, v: &str) {
        let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
        unsafe { (self.api.set_prop_str)(self.h, k.as_ptr(), v.as_ptr()) };
    }

    fn cmd(&self, args: &[&str]) {
        let c: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
        let mut p: Vec<*const c_char> = c.iter().map(|a| a.as_ptr()).collect();
        p.push(std::ptr::null());
        unsafe { (self.api.command_async)(self.h, 0, p.as_ptr()) };
    }

    fn get_f(&self, k: &str) -> Option<f64> {
        let k = CString::new(k).unwrap();
        let mut v = 0f64;
        let r = unsafe { (self.api.get_prop)(self.h, k.as_ptr(), FMT_DOUBLE, &mut v as *mut f64 as *mut c_void) };
        (r >= 0).then_some(v)
    }

    fn get_s(&self, k: &str) -> Option<String> {
        let k = CString::new(k).unwrap();
        unsafe {
            let p = (self.api.get_prop_str)(self.h, k.as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.api.free)(p as *mut c_void);
            Some(s)
        }
    }

    pub fn play(&mut self, t: Target) {
        self.st.msg.clear();
        self.st.hw.clear();
        self.st.playing = true;
        self.pending = Some(t);
        if !self.rc.is_null() {
            self.start_pending();
        }
    }

    fn start_pending(&mut self) {
        let Some(t) = self.pending.take() else { return };
        self.st.tracks.clear();
        self.st.pos = 0.0;
        self.st.dur = 0.0;
        self.set("pause", "no");
        match &t {
            Target::File(p) => {
                self.set("start", "none");
                self.set("length", "none");
                self.cmd(&["loadfile", p, "replace"]);
            }
            Target::Playlist {
                device,
                name,
                start,
                length,
                ..
            } => {
                #[cfg(feature = "bluray")]
                if let Target::Playlist { bd: Some(bd), segs, .. } = &t {
                    if !segs.is_empty() {
                        *cb::NEXT.lock().unwrap() = Some((bd.clone(), segs.clone()));
                        self.set("start", "none");
                        self.set("length", "none");
                        self.cmd(&["loadfile", "tuxbd://playlist", "replace"]);
                        return;
                    }
                }
                self.set("bluray-device", device);
                self.set("start", &if *start > 0.5 { format!("{start:.3}") } else { "none".into() });
                self.set("length", &length.map_or("none".into(), |l| format!("{l:.3}")));
                let n = name.trim_end_matches(".MPLS").trim_end_matches(".mpls");
                self.cmd(&["loadfile", &format!("bd://mpls/{n}"), "replace"]);
            }
        }
    }

    pub fn stop(&mut self) {
        self.pending = None;
        self.cmd(&["stop"]);
        self.st.playing = false;
        self.st.pos = 0.0;
        self.st.dur = 0.0;
        self.st.tracks.clear();
    }

    pub fn toggle_pause(&mut self) {
        self.set("pause", if self.st.paused { "no" } else { "yes" });
    }

    pub fn seek(&mut self, secs: f64) {
        self.cmd(&["seek", &format!("{secs:.3}"), "absolute"]);
    }

    pub fn set_volume(&mut self, v: f64) {
        self.set("volume", &format!("{v:.0}"));
        self.st.volume = v;
    }

    /// kind is "audio" or "sub"; id 0 turns it off.
    pub fn select(&mut self, kind: &str, id: i64) {
        self.set(
            if kind == "audio" { "aid" } else { "sid" },
            &if id == 0 { "no".to_string() } else { id.to_string() },
        );
        for t in self.st.tracks.iter_mut().filter(|t| t.kind == kind) {
            t.selected = t.id == id;
        }
    }

    fn read_tracks(&mut self) {
        let n = self.get_s("track-list/count").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        let mut v = Vec::new();
        for i in 0..n {
            let g = |f: &str| self.get_s(&format!("track-list/{i}/{f}")).unwrap_or_default();
            let kind = g("type");
            if kind != "audio" && kind != "sub" {
                continue;
            }
            let id = g("id").parse().unwrap_or(0);
            let mut label = format!("{id}");
            for part in [g("lang"), g("codec"), g("title")] {
                if !part.is_empty() {
                    label.push_str(" - ");
                    label.push_str(&part);
                }
            }
            v.push(Track {
                kind,
                id,
                label,
                selected: g("selected") == "yes",
            });
        }
        self.st.tracks = v;
    }

    /// Reads mpv's events and refreshes the playback state; call once per UI frame.
    pub fn poll(&mut self) {
        if !self.rc.is_null() {
            self.start_pending();
        }
        loop {
            let e = unsafe { &*(self.api.wait_event)(self.h, 0.0) };
            match e.id {
                0 => break,
                EV_LOADED => self.read_tracks(),
                EV_END if !e.data.is_null() => {
                    let end = unsafe { &*(e.data as *const EndFile) };
                    if end.reason == 4 {
                        let m = unsafe { cstr((self.api.err)(end.error)) };
                        self.st.msg = format!("Playback failed: {m}");
                        self.st.playing = false;
                    }
                }
                EV_LOG if !e.data.is_null() => {
                    let l = unsafe { &*(e.data as *const LogMsg) };
                    let (p, t) = unsafe { (cstr(l.prefix), cstr(l.text)) };
                    let t = t.trim();
                    if !t.is_empty() && matches!(p.as_str(), "bd" | "libbluray" | "ffmpeg" | "cplayer" | "demux" | "stream" | "file") {
                        self.st.msg = format!("[{p}] {t}");
                    }
                }
                _ => {}
            }
        }
        #[cfg(feature = "bluray")]
        if let Ok(mut e) = cb::ERR.lock() {
            if !e.is_empty() {
                self.st.msg = std::mem::take(&mut *e);
            }
        }
        if self.st.playing {
            self.st.hw = self.get_s("hwdec-current").unwrap_or_default();
            self.st.pos = self.get_f("time-pos").unwrap_or(self.st.pos);
            self.st.dur = self.get_f("duration").unwrap_or(self.st.dur);
            let mut f: c_int = 0;
            let k = CString::new("pause").unwrap();
            if unsafe { (self.api.get_prop)(self.h, k.as_ptr(), FMT_FLAG, &mut f as *mut c_int as *mut c_void) } >= 0 {
                self.st.paused = f != 0;
            }
        }
    }

    /// Called from the GUI's paint callback (OpenGL context current): renders the current frame into the
    /// area `info` describes, on top of whatever egui has drawn so far.
    pub fn paint(&mut self, info: &eframe::egui::PaintCallbackInfo, gl: &eframe::glow::Context) {
        use eframe::glow::{self, HasContext};
        let vp = info.viewport_in_pixels();
        let (w, h) = (vp.width_px.max(1), vp.height_px.max(1));
        unsafe {
            if self.rc.is_null() && !self.rc_failed {
                if !find_proc() {
                    self.rc_failed = true;
                    self.st.msg = "Could not find the OpenGL loader for the preview.".into();
                    return;
                }
                let api = CString::new("opengl").unwrap();
                let mut init = InitParams {
                    get_proc_address: get_proc,
                    ctx: null_mut(),
                };
                let mut ps = [
                    Param {
                        ty: P_API,
                        data: api.as_ptr() as *mut c_void,
                    },
                    Param {
                        ty: P_INIT,
                        data: &mut init as *mut InitParams as *mut c_void,
                    },
                    Param { ty: 0, data: null_mut() },
                ];
                let r = (self.api.rc_create)(&mut self.rc, self.h, ps.as_mut_ptr());
                if r < 0 {
                    self.rc = null_mut();
                    self.rc_failed = true;
                    self.st.msg = format!("mpv could not set up OpenGL rendering: {}", cstr((self.api.err)(r)));
                    return;
                }
            }
            if self.rc.is_null() {
                return;
            }
            // egui's framebuffer, to hand back untouched (read before we bind our own)
            let prev = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32;
            // (re)create the off-screen target when the area changed size
            if self.fbo.is_none_or(|(_, _, fw, fh)| fw != w || fh != h) {
                if let Some((f, t, _, _)) = self.fbo.take() {
                    gl.delete_framebuffer(f);
                    gl.delete_texture(t);
                }
                let (Ok(t), Ok(f)) = (gl.create_texture(), gl.create_framebuffer()) else {
                    return;
                };
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    w,
                    h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
                gl.bind_framebuffer(glow::FRAMEBUFFER, std::num::NonZeroU32::new(prev).map(glow::NativeFramebuffer));
                self.fbo = Some((f, t, w, h));
            }
            let (f, _, _, _) = self.fbo.unwrap();
            let mut fb = FboParam {
                fbo: f.0.get() as c_int,
                w,
                h,
                internal_format: 0,
            };
            let mut flip: c_int = 1;
            let mut ps = [
                Param {
                    ty: P_FBO,
                    data: &mut fb as *mut FboParam as *mut c_void,
                },
                Param {
                    ty: P_FLIP,
                    data: &mut flip as *mut c_int as *mut c_void,
                },
                Param { ty: 0, data: null_mut() },
            ];
            // lets mpv pick up the newest decoded frame; without this it keeps drawing black
            (self.api.rc_update)(self.rc);
            (self.api.rc_render)(self.rc, ps.as_mut_ptr());
            // copy it to the window, in the spot egui reserved
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(f));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, std::num::NonZeroU32::new(prev).map(glow::NativeFramebuffer));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                vp.left_px,
                vp.from_bottom_px,
                vp.left_px + w,
                vp.from_bottom_px + h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, std::num::NonZeroU32::new(prev).map(glow::NativeFramebuffer));
        }
    }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
