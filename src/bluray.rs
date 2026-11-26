//! Optional decryption through an installed libbluray (loaded at run time, nothing is linked).
//! libbluray finds libaacs/libbdplus on its own, so MakeMKV's libmmbd works here exactly as it does for VLC.

use std::ffi::{c_char, c_void, CString};
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[repr(C)]
struct BdFile {
    internal: *mut c_void,
    close: unsafe extern "C" fn(*mut BdFile),
    seek: unsafe extern "C" fn(*mut BdFile, i64, i32) -> i64,
    tell: unsafe extern "C" fn(*mut BdFile) -> i64,
    eof: *const c_void, // may be NULL; never called
    read: unsafe extern "C" fn(*mut BdFile, *mut u8, i64) -> i64,
    write: unsafe extern "C" fn(*mut BdFile, *const u8, i64) -> i64,
}

#[cfg(unix)]
mod sys {
    use std::ffi::{c_char, c_int, c_void};
    extern "C" {
        pub fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
        pub fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    pub unsafe fn load(name: *const c_char) -> *mut c_void {
        dlopen(name, 2)
    }
    pub unsafe fn sym(h: *mut c_void, name: *const c_char) -> *mut c_void {
        dlsym(h, name)
    }
    pub const NAMES: &[&str] = &["libbluray.so.2", "libbluray.so", "libbluray.2.dylib", "libbluray.dylib"];
}
#[cfg(windows)]
mod sys {
    use std::ffi::{c_char, c_void};
    extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    pub unsafe fn load(name: *const c_char) -> *mut c_void {
        LoadLibraryA(name)
    }
    pub unsafe fn sym(h: *mut c_void, name: *const c_char) -> *mut c_void {
        GetProcAddress(h, name)
    }
    pub const NAMES: &[&str] = &["libbluray-2.dll", "libbluray.dll", "bluray.dll"];
}

type OpenFn = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void;
type CloseFn = unsafe extern "C" fn(*mut c_void);
type OpenFileDecFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut BdFile;

/// An open disc in libbluray. Shared between the scan and the preview player, so every call into
/// libbluray happens under `lock`.
pub struct Disc {
    bd: *mut c_void,
    close: CloseFn,
    open_file_dec: OpenFileDecFn,
    lock: Mutex<()>,
}

// SAFETY: libbluray handles may be used from any thread as long as calls do not overlap; every call
// that touches `bd` or a file handle taken from it goes through `lock`.
unsafe impl Send for Disc {}
unsafe impl Sync for Disc {}

impl Drop for Disc {
    fn drop(&mut self) {
        // SAFETY: `bd` came from `bd_open` and is closed exactly once; every `Reader` holds an `Arc<Disc>`,
        // so no file handle outlives it.
        unsafe { (self.close)(self.bd) }
    }
}

fn cstr(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| "invalid path".to_string())
}

/// Make libbluray use MakeMKV's libmmbd for AACS/BD+ when it is installed. libbluray tries
/// libaacs first and only falls back to libmmbd if libaacs is missing, so an installed libaacs
/// without a key file would otherwise win. libbluray's LIBAACS_PATH / LIBBDPLUS_PATH override the
/// library name (it appends ".so.0"). Set BDVIEW_PLUS_NO_MMBD to skip, or LIBAACS_PATH yourself to override.
#[cfg(unix)]
fn find_mmbd() -> Option<String> {
    use std::env::var_os;
    if var_os("BDVIEW_PLUS_NO_MMBD").is_some() {
        return None;
    }
    let mut dirs: Vec<String> = var_os("BDVIEW_PLUS_MMBD")
        .map(|v| v.to_string_lossy().into_owned())
        .into_iter()
        .collect();
    dirs.extend(
        [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
            "/opt/makemkv/lib",
            "/opt/makemkv/lib64",
        ]
        .iter()
        .map(|d| d.to_string()),
    );
    if let Some(d) = dirs.iter().find(|d| Path::new(d).join("libmmbd.so.0").exists()) {
        return Some(format!("{d}/libmmbd"));
    }
    // not in a known directory: use it only if the dynamic loader can find it
    let h = unsafe { sys::load(c"libmmbd.so.0".as_ptr()) };
    (!h.is_null()).then(|| "libmmbd".to_string())
}
#[cfg(not(unix))]
fn find_mmbd() -> Option<String> {
    None
}

/// True when MakeMKV's libmmbd will be used (opening a disc then waits for MakeMKV to start).
pub fn mmbd_found() -> bool {
    !DISABLE.load(std::sync::atomic::Ordering::Relaxed) && std::env::var_os("LIBAACS_PATH").is_none() && find_mmbd().is_some()
}

pub(crate) fn prefer_mmbd() {
    use std::env::{set_var, var_os};
    if var_os("LIBAACS_PATH").is_some() {
        return;
    }
    if let Some(base) = find_mmbd() {
        set_var("LIBAACS_PATH", &base);
        if var_os("LIBBDPLUS_PATH").is_none() {
            set_var("LIBBDPLUS_PATH", &base);
        }
    }
}

/// Load libbluray (override the name with BDVIEW_PLUS_LIBBLURAY) and open the disc at `path`.
pub fn open(path: &Path) -> Result<Arc<Disc>, String> {
    prefer_mmbd();
    unsafe {
        let mut names: Vec<String> = std::env::var("BDVIEW_PLUS_LIBBLURAY").into_iter().collect();
        names.extend(sys::NAMES.iter().map(|s| s.to_string()));
        let h = names.iter().find_map(|n| {
            let h = sys::load(cstr(n).ok()?.as_ptr());
            (!h.is_null()).then_some(h)
        });
        let h = h.ok_or("libbluray not found (install libbluray, or set BDVIEW_PLUS_LIBBLURAY to its path)")?;
        let get = |n: &str| {
            let p = sys::sym(h, cstr(n).unwrap().as_ptr());
            if p.is_null() {
                Err(format!("libbluray has no {n}"))
            } else {
                Ok(p)
            }
        };
        let open = std::mem::transmute::<*mut std::ffi::c_void, OpenFn>(get("bd_open")?);
        let close = std::mem::transmute::<*mut std::ffi::c_void, CloseFn>(get("bd_close")?);
        let open_file_dec = std::mem::transmute::<*mut std::ffi::c_void, OpenFileDecFn>(get("bd_open_file_dec")?);
        if let Ok(p) = get("bd_set_debug_mask") {
            // quiet on stderr, unless BDVIEW_PLUS_TIMING is set: then libbluray prints everything it logs
            let mask = if std::env::var_os("BDVIEW_PLUS_TIMING").is_some() {
                0xFFFF_FFFF
            } else {
                0
            };
            std::mem::transmute::<*mut std::ffi::c_void, unsafe extern "C" fn(u32)>(p)(mask);
            if mask != 0 {
                eprintln!(
                    "bdview-plus: LIBAACS_PATH={:?} LIBBDPLUS_PATH={:?}",
                    std::env::var("LIBAACS_PATH").ok(),
                    std::env::var("LIBBDPLUS_PATH").ok()
                );
            }
        }
        let bd = open(cstr(&path.to_string_lossy())?.as_ptr(), std::ptr::null());
        if bd.is_null() {
            return Err(format!("libbluray could not open {}", path.display()));
        }
        Ok(Arc::new(Disc {
            bd,
            close,
            open_file_dec,
            lock: Mutex::new(()),
        }))
    }
}

/// Set by --no-bluray: read the stream files directly and never load libbluray.
pub static DISABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const UNIT: usize = 6144;

/// Decrypting reader for one stream file.
pub struct Reader {
    f: *mut BdFile,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    done: bool,
    /// Bytes to drop from the first block after a seek into the middle of a unit.
    skip: usize,
    _d: Arc<Disc>,
}

impl Disc {
    /// Open `BDMV/STREAM/<name>` through libbluray's decryption layer.
    pub fn open_stream(self: &Arc<Self>, name: &str) -> io::Result<Reader> {
        let bad = |m: &str| io::Error::other(m.to_string());
        let path = cstr(&format!("BDMV/STREAM/{name}")).map_err(|e| bad(&e))?;
        let f = {
            let _g = self.lock.lock().unwrap();
            // SAFETY: `bd` is valid for the life of `self`, `path` is NUL-terminated, calls are serialised by `lock`.
            unsafe { (self.open_file_dec)(self.bd, path.as_ptr()) }
        };
        if f.is_null() {
            return Err(bad("libbluray could not open the stream file (is AACS decryption set up?)"));
        }
        Ok(Reader {
            f,
            buf: vec![0; UNIT * 32],
            pos: 0,
            end: 0,
            done: false,
            skip: 0,
            _d: self.clone(),
        })
    }
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.end {
            if self.done {
                return Ok(0);
            }
            // libbluray's decrypting file handle accepts only reads of exactly one 6144-byte unit,
            // so fill the buffer one unit at a time.
            let mut got = 0usize;
            let mut failed = false;
            while got + UNIT <= self.buf.len() {
                let n = {
                    let _g = self._d.lock.lock().unwrap();
                    // SAFETY: `f` is a live handle owned by this Reader; the destination slice has at least UNIT bytes left.
                    unsafe { ((*self.f).read)(self.f, self.buf[got..].as_mut_ptr(), UNIT as i64) }
                };
                if n < 0 {
                    failed = true;
                    break;
                }
                if n == 0 {
                    self.done = true;
                    break;
                }
                got += n as usize;
            }
            if failed && got == 0 {
                return Err(io::Error::other("libbluray read failed"));
            }
            (self.pos, self.end) = (self.skip.min(got), got);
            self.skip = 0;
            if got == 0 {
                return Ok(0);
            }
        }
        let k = out.len().min(self.end - self.pos);
        out[..k].copy_from_slice(&self.buf[self.pos..self.pos + k]);
        self.pos += k;
        Ok(k)
    }
}

impl Reader {
    /// Continue reading at byte `at` of the stream file.
    fn seek(&mut self, at: u64) -> bool {
        let unit = at / UNIT as u64 * UNIT as u64;
        let r = {
            let _g = self._d.lock.lock().unwrap();
            // SAFETY: `f` is a live handle owned by this Reader; calls are serialised by `lock`.
            unsafe { ((*self.f).seek)(self.f, unit as i64, 0) }
        };
        if r < 0 {
            return false;
        }
        (self.pos, self.end, self.done, self.skip) = (0, 0, false, (at - unit) as usize);
        true
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _g = self._d.lock.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: `f` is closed exactly once, here, while the Disc that produced it is still alive.
        unsafe { ((*self.f).close)(self.f) }
    }
}

/// True when the first source packets of a stream file are scrambled (no 0x47 sync byte after the 4-byte header).
pub fn scrambled(head: &[u8]) -> bool {
    head.len() >= 197 && !(head[4] == 0x47 && head[196] == 0x47)
}

/// A playlist as one stream: the byte ranges of its clips (see `disc::Seg`), read one after the other
/// through the open libbluray handle, so the data is decrypted by the same AACS / MakeMKV session the scan
/// uses. The preview plays from this instead of mpv's own `bd://` support, which opens the disc a second time and
/// makes libbluray list every playlist and clip info file on the disc first.
pub struct Stream {
    d: Arc<Disc>,
    segs: Vec<crate::disc::Seg>,
    /// Offset of each segment in the whole stream.
    starts: Vec<u64>,
    total: u64,
    cur: Option<(usize, Reader)>,
    /// Bytes left in the current segment.
    left: u64,
    at: u64,
}

impl Disc {
    pub fn stream(self: &Arc<Self>, segs: Vec<crate::disc::Seg>) -> Stream {
        let mut starts = Vec::new();
        let mut total = 0;
        for s in &segs {
            starts.push(total);
            total += s.end.saturating_sub(s.start);
        }
        Stream {
            d: self.clone(),
            segs,
            starts,
            total,
            cur: None,
            left: 0,
            at: 0,
        }
    }
}

impl Stream {
    pub fn size(&self) -> i64 {
        self.total as i64
    }

    /// Move to byte `at` of the stream; returns it, or -1.
    pub fn seek(&mut self, at: i64) -> i64 {
        if at < 0 || at as u64 > self.total {
            return -1;
        }
        let at = at as u64;
        self.cur = None;
        self.left = 0;
        self.at = at;
        let Some(i) = (0..self.segs.len())
            .rev()
            .find(|&i| self.starts[i] <= at && at < self.starts[i] + self.segs[i].end - self.segs[i].start)
        else {
            return at as i64; // the very end
        };
        let seg = &self.segs[i];
        let into = at - self.starts[i];
        let Ok(mut r) = self.d.open_stream(&seg.name) else { return -1 };
        if !r.seek(seg.start + into) {
            return -1;
        }
        self.left = seg.end - seg.start - into;
        self.cur = Some((i, r));
        at as i64
    }

    pub fn read(&mut self, out: &mut [u8]) -> i64 {
        loop {
            if self.cur.is_none() && self.seek(self.at as i64) < 0 {
                return -1;
            }
            let Some((i, r)) = self.cur.as_mut() else { return 0 }; // past the last segment
            if self.left > 0 {
                let k = (out.len() as u64).min(self.left) as usize;
                match r.read(&mut out[..k]) {
                    Ok(n) if n > 0 => {
                        self.left -= n as u64;
                        self.at += n as u64;
                        return n as i64;
                    }
                    Ok(_) => self.left = 0, // the file ended early: go on with the next segment
                    Err(_) => return -1,
                }
            }
            // segment finished: the next one starts where this one's nominal length ends
            let next = *i + 1;
            if next >= self.segs.len() {
                self.cur = None;
                self.at = self.total;
                return 0;
            }
            self.at = self.starts[next];
            self.cur = None;
        }
    }
}
