//! Native window (egui/eframe) with the classic three-list layout. A worker thread owns the disc
//! and does all the scanning; the UI only reads shared snapshots.

use crate::disc::Disc;
use crate::report::{self, n0};
use crate::scan::{self, Mode};
use crate::stream::Stream;
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

enum Cmd {
    Open {
        path: String,
        saved: bool,
        extended: bool,
    },
    /// The disc was removed: forget everything about it (and release the drive).
    Close,
    Scan {
        names: Vec<String>,
        extended: bool,
        save: bool,
    },
    Report {
        names: Vec<String>,
        diag: bool,
        summary: bool,
    },
    Chart {
        name: String,
        pid: u16,
        angle: usize,
        kind: usize,
    },
    /// A user playlist built from (playlist name, clip index) pairs.
    Custom {
        name: String,
        items: Vec<(String, usize)>,
    },
}

struct ClipRow {
    name: String,
    index: usize,
    len: f64,
    est: u64,
    meas: u64,
    /// The part of the stream file the playlist plays (for the preview).
    seg: Option<crate::disc::Seg>,
    /// (PID, stream type) of the streams its clip info lists, for the custom playlist's compatibility filter.
    types: Vec<(u16, u8)>,
}

struct StreamRow {
    codec: String,
    lang: String,
    rate: String,
    desc: String,
}

struct PlRow {
    name: String,
    group: usize,
    len: f64,
    loops: bool,
    est: u64,
    meas: u64,
    clips: Vec<ClipRow>,
    streams: Vec<StreamRow>,
    /// Video streams (PID, label) and the number of extra angles, for the chart pickers.
    vids: Vec<(u16, String)>,
    angles: usize,
}

#[derive(Default)]
struct Shared {
    busy: bool,
    label: String,
    done: u64,
    total: u64,
    elapsed: f64,
    remaining: f64,
    log: Vec<String>,
    rows: Arc<Vec<PlRow>>,
    rows_ver: u64,
    report: String,
    report_ver: u64,
    /// Set when a scan finishes; the UI shows it in a popup once.
    scan_done: Option<String>,
    /// Latest chart computed by the worker, and a note when none could be made.
    chart: Option<Arc<crate::chart::Chart>>,
    chart_ver: u64,
    chart_note: Option<String>,
    /// Name of a custom playlist just added (the UI ticks it).
    custom_added: Option<String>,
    /// The open libbluray handle, for the preview player.
    #[cfg(feature = "bluray")]
    bd: Option<Arc<crate::bluray::Disc>>,
}

type Sh = Arc<Mutex<Shared>>;

fn stream_row(s: &Stream) -> StreamRow {
    let rate = if s.bitrate <= 0 {
        "-".to_string()
    } else if s.is_video() || s.is_audio() {
        format!("{} kbps", report::kbps_int(s.bitrate))
    } else {
        format!("{} kbps", (s.bitrate as f64 / 1000.0).round() as i64)
    };
    StreamRow {
        codec: format!("{}{}", if s.hidden { "* " } else { "" }, s.codec_name()),
        lang: s.lang_name.clone(),
        rate,
        desc: s.description(),
    }
}

/// Playlists sorted longest first, with the "Group" number (playlists that share clips).
fn snapshot(d: &Disc) -> Vec<PlRow> {
    let mut idx: Vec<usize> = (0..d.playlists.len()).collect();
    idx.sort_by(|&a, &b| {
        let (x, y) = (&d.playlists[a], &d.playlists[b]);
        y.total_length().total_cmp(&x.total_length()).then_with(|| x.name.cmp(&y.name))
    });
    let (mut groups, mut next) = (HashMap::<&str, usize>::new(), 1usize);
    idx.iter()
        .map(|&i| {
            let p = &d.playlists[i];
            let g = p
                .clips
                .iter()
                .find_map(|c| groups.get(c.name.as_str()).copied())
                .unwrap_or_else(|| {
                    next += 1;
                    next - 1
                });
            for c in &p.clips {
                groups.entry(c.name.as_str()).or_insert(g);
            }
            PlRow {
                name: p.name.clone(),
                group: g,
                len: p.total_length(),
                loops: p.has_loops,
                est: p.file_size(),
                meas: p.clips.iter().map(|c| c.packet_size()).sum(),
                clips: p
                    .clips
                    .iter()
                    .map(|c| ClipRow {
                        name: if c.angle > 0 {
                            format!("{} ({})", c.name, c.angle)
                        } else {
                            c.name.clone()
                        },
                        index: c.angle + 1,
                        len: c.length,
                        est: c.file_size,
                        meas: c.packet_size(),
                        seg: d.segment(c),
                        types: d
                            .clpis
                            .get(&c.clpi)
                            .map_or(Vec::new(), |s| s.list.iter().map(|x| (x.pid, x.stype)).collect()),
                    })
                    .collect(),
                streams: p.sorted().into_iter().map(stream_row).collect(),
                vids: p
                    .streams
                    .list
                    .iter()
                    .filter(|s| s.is_video())
                    .map(|s| (s.pid, format!("{} (PID {})", s.codec_name(), s.pid)))
                    .collect(),
                angles: p.angle_count,
            }
        })
        .collect()
}

fn disc_log(d: &Disc) -> Vec<String> {
    let mut v = vec![format!("Detected BDMV Folder: {} ({})", d.source, d.label)];
    for (f, n) in [
        (d.is_bdplus, "BD+ detected."),
        (d.is_bdjava, "BD-Java detected."),
        (d.is_uhd, "Ultra HD detected."),
        (d.is_50hz, "50Hz content detected."),
        (d.is_3d, "Blu-ray 3D detected."),
        (d.is_dbox, "D-BOX motion code detected."),
        (d.is_psp, "PSP Digital Copy detected."),
    ] {
        if f {
            v.push(n.into());
        }
    }
    v.push(format!("Disc Size: {} bytes", n0(d.size as i64)));
    v.extend(d.errors.iter().map(|e| format!("Warning: {e}")));
    v
}

fn worker(rx: Receiver<Cmd>, sh: Sh, cancel: Arc<AtomicBool>, ctx: egui::Context) {
    let mut disc: Option<Disc> = None;
    let mut warnings: Vec<String> = Vec::new();
    for cmd in rx {
        cancel.store(false, Ordering::Relaxed);
        let t0 = Instant::now();
        let begin = |label: &str| {
            let mut u = sh.lock().unwrap();
            u.busy = true;
            u.label = label.into();
            (u.done, u.total, u.elapsed, u.remaining) = (0, 0, 0.0, 0.0);
        };
        let mut progress = |p: &scan::Progress| {
            let mut u = sh.lock().unwrap();
            u.label = if p.file.is_empty() {
                format!("{} done", p.label)
            } else {
                format!("{} {}", p.label, p.file)
            };
            (u.done, u.total) = (p.done, p.total);
            u.elapsed = t0.elapsed().as_secs_f64();
            u.remaining = if p.done > 0 {
                u.elapsed * (p.total - p.done) as f64 / p.done as f64
            } else {
                0.0
            };
            drop(u);
            ctx.request_repaint();
            !cancel.load(Ordering::Relaxed)
        };
        match cmd {
            Cmd::Open { path, saved, extended } => {
                #[cfg(feature = "bluray")]
                let waiting = crate::bluray::mmbd_found();
                #[cfg(not(feature = "bluray"))]
                let waiting = false;
                begin(if waiting {
                    "Opening disc - waiting for MakeMKV to start (up to 30 s)"
                } else {
                    "Opening"
                });
                {
                    let mut u = sh.lock().unwrap();
                    u.log.clear();
                    u.rows = Arc::new(Vec::new());
                    u.rows_ver += 1;
                    #[cfg(feature = "bluray")]
                    {
                        u.bd = None;
                    }
                }
                disc = None;
                {
                    let (sh2, ctx2) = (sh.clone(), ctx.clone());
                    crate::disc::on_step(Some(Arc::new(move |t: &str| {
                        sh2.lock().unwrap().label = t.into();
                        ctx2.request_repaint();
                    })));
                }
                let opened = Disc::open(Path::new(path.trim()));
                crate::disc::on_step(None);
                match opened {
                    Ok(mut d) => {
                        // structure only (playlists, clips, streams from the clip info files);
                        // stream data is read later by Scan Bitrates
                        d.finish();
                        let found = saved && crate::scancache::exists(&d);
                        if found {
                            let mut u = sh.lock().unwrap();
                            u.label = "Found a saved bitrate scan - loading it".into();
                            u.log.push("Found a saved bitrate scan for this disc, loading it...".into());
                            drop(u);
                            ctx.request_repaint();
                        }
                        let loaded = if saved { crate::scancache::load(&mut d, extended) } else { None };
                        let mut u = sh.lock().unwrap();
                        u.log.extend(disc_log(&d));
                        u.log.extend(warnings.drain(..).map(|w| format!("Warning: {w}")));
                        if found && loaded.is_none() {
                            u.log.push("The saved bitrate scan was not used (it was made with different settings, or the file is damaged). Press Scan Bitrates to measure again.".into());
                        }
                        if let Some(l) = loaded {
                            u.log.push(format!(
                                "Loaded the saved bitrate scan from {} ({} playlist{}). Press Scan Bitrates to measure again.",
                                crate::scancache::date(l.when),
                                l.playlists,
                                if l.playlists == 1 { "" } else { "s" }
                            ));
                            u.log.extend(l.warnings.iter().map(|w| format!("Warning: {w}")));
                            warnings = l.warnings;
                        }
                        #[cfg(feature = "bluray")]
                        {
                            u.bd = d.bd.clone();
                        }
                        disc = Some(d);
                    }
                    Err(e) => sh.lock().unwrap().log.push(format!("Error: {e}")),
                }
                let mut u = sh.lock().unwrap();
                u.busy = false;
                u.label.clear();
                (u.done, u.total) = (0, 0);
                if let Some(d) = &disc {
                    u.rows = Arc::new(snapshot(d));
                }
                u.rows_ver += 1;
            }
            Cmd::Close => {
                disc = None;
                warnings.clear();
                let mut u = sh.lock().unwrap();
                *u = Shared {
                    rows_ver: u.rows_ver + 1,
                    report_ver: u.report_ver,
                    chart_ver: u.chart_ver,
                    ..Default::default()
                };
            }
            Cmd::Scan { names, extended, save } => {
                let Some(d) = disc.as_mut() else { continue };
                crate::hevc::EXTENDED.store(extended, Ordering::Relaxed);
                let sel: Vec<usize> = (0..d.playlists.len()).filter(|&i| names.contains(&d.playlists[i].name)).collect();
                begin("Scanning");
                warnings = scan::scan(d, &sel, Mode::Both, &mut progress);
                let scanned = !warnings.iter().any(|w| w.contains("cancelled"));
                let mut u = sh.lock().unwrap();
                u.busy = false;
                u.label.clear();
                u.elapsed = t0.elapsed().as_secs_f64();
                u.remaining = 0.0;
                if !scanned {
                    u.log.push("Scan cancelled.".into());
                } else {
                    let t = u.elapsed as u64;
                    let took = format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60);
                    u.scan_done = Some(if warnings.is_empty() {
                        format!("The bitrate scan finished in {took}.\n\nPress View Report to see the results.")
                    } else {
                        format!("The bitrate scan finished in {took} with {} warning(s); they are listed in the log.\n\nPress View Report to see the results.", warnings.len())
                    });
                }
                if scanned && save {
                    let done: Vec<String> = sel.iter().map(|&i| d.playlists[i].name.clone()).collect();
                    match crate::scancache::save(d, extended, &done, &warnings) {
                        Ok(p) => u.log.push(format!("Saved the scan to {}", p.display())),
                        Err(e) => u.log.push(format!("Warning: could not save the scan: {e}")),
                    }
                }
                if warnings.is_empty() {
                    u.log.push("Scan complete.".into());
                } else {
                    u.log.extend(warnings.iter().map(|w| format!("Warning: {w}")));
                }
                u.rows = Arc::new(snapshot(d));
                u.rows_ver += 1;
            }
            Cmd::Report { names, diag, summary } => {
                let Some(d) = disc.as_ref() else { continue };
                let sel: Vec<usize> = names.iter().filter_map(|n| d.playlists.iter().position(|p| &p.name == n)).collect();
                let mut text = report::generate(
                    d,
                    &sel,
                    &report::Options {
                        diag,
                        summary,
                        warnings: warnings.clone(),
                    },
                );
                let unscanned: Vec<&str> = sel
                    .iter()
                    .map(|&i| &d.playlists[i])
                    .filter(|p| p.clips.iter().any(|c| c.has_file) && !p.clips.iter().any(|c| c.packet_count > 0))
                    .map(|p| p.name.as_str())
                    .collect();
                if !unscanned.is_empty() {
                    let which = if unscanned.len() == sel.len() {
                        "the stream files have not been scanned".to_string()
                    } else {
                        format!("{} have not been scanned", unscanned.join(", "))
                    };
                    text.insert_str(0, &format!("NOTE: {which} (or the scan was cancelled), so sizes, bit rates and\nstream details for them are empty. Press Scan Bitrates first, then View Report.\n\n"));
                }
                let mut u = sh.lock().unwrap();
                u.report = text;
                u.report_ver += 1;
            }
            Cmd::Chart { name, pid, angle, kind } => {
                let Some(d) = disc.as_ref() else { continue };
                let Some(pl) = d.playlists.iter().find(|p| p.name == name) else {
                    continue;
                };
                let mut u = sh.lock().unwrap();
                if crate::chart::has_data(d, pl, pid, angle) {
                    u.chart = Some(Arc::new(crate::chart::build(d, pl, kind, pid, angle)));
                    u.chart_note = None;
                    u.chart_ver += 1;
                } else {
                    u.chart_note = Some(format!(
                        "No bitrate data for {name} / PID {pid} / angle {angle}: press Scan Bitrates (with this playlist selected) first."
                    ));
                    u.chart_ver += 1;
                }
            }
            Cmd::Custom { name, items } => {
                let Some(d) = disc.as_mut() else { continue };
                let mut u = sh.lock().unwrap();
                if d.add_custom(&name, &items).is_some() {
                    u.log.push(format!(
                        "Custom playlist {name} added ({} stream files). Press Scan Bitrates to measure it.",
                        items.len()
                    ));
                    u.custom_added = Some(name);
                    u.rows = Arc::new(snapshot(d));
                    u.rows_ver += 1;
                }
            }
        }
        ctx.request_repaint();
    }
}

fn hms(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Sizes as the original shows them: plain bytes, or binary GB / MB / KB when "human readable" is on.
fn num(n: u64, human: bool) -> String {
    if n == 0 {
        return "-".into();
    }
    if !human {
        return n0(n as i64);
    }
    let f = n as f64;
    if f >= 1073741824.0 {
        format!("{:.2} GB", f / 1073741824.0)
    } else if f >= 1048576.0 {
        format!("{:.2} MB", f / 1048576.0)
    } else if f >= 1024.0 {
        format!("{:.2} KB", f / 1024.0)
    } else {
        format!("{n} bytes")
    }
}

#[derive(Clone)]
struct Settings {
    filter_short: bool,
    short_secs: f32,
    filter_loops: bool,
    human: bool,
    diag: bool,
    summary: bool,
    extended: bool,
    /// Save bitrate scans and reuse them when the same disc is opened again.
    use_saved: bool,
    /// Text size for the whole window, in points.
    font: f32,
    /// Text size in the report window.
    report_font: f32,
    report_wrap: bool,
    dark: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            filter_short: true,
            short_secs: 20.0,
            filter_loops: true,
            human: true,
            diag: true,
            summary: true,
            extended: false,
            use_saved: true,
            font: 16.0,
            report_font: 16.0,
            report_wrap: true,
            dark: false,
        }
    }
}

fn cfg_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    let new = base.join("tuxinfo").join("settings.txt");
    Some(new)
}

impl Settings {
    fn load() -> Settings {
        let mut s = Settings::default();
        let Some(text) = cfg_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return s;
        };
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let (k, v) = (k.trim(), v.trim());
            let b = v == "true";
            match k {
                "filter_short" => s.filter_short = b,
                "short_secs" => s.short_secs = v.parse().unwrap_or(s.short_secs),
                "filter_loops" => s.filter_loops = b,
                "human" => s.human = b,
                "diag" => s.diag = b,
                "summary" => s.summary = b,
                "extended" => s.extended = b,
                "use_saved" => s.use_saved = b,
                "font" => s.font = v.parse().unwrap_or(16.0f32).clamp(9.0, 40.0),
                "report_wrap" => s.report_wrap = b,
                "dark" => s.dark = b,
                "report_font" => s.report_font = v.parse().unwrap_or(16.0f32).clamp(8.0, 48.0),
                _ => {}
            }
        }
        s
    }
    fn save(&self) {
        let Some(p) = cfg_path() else { return };
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let t = format!(
            "filter_short={}\nshort_secs={}\nfilter_loops={}\nhuman={}\ndiag={}\nsummary={}\nextended={}\nuse_saved={}\nfont={}\nreport_font={}\nreport_wrap={}\ndark={}\n",
            self.filter_short, self.short_secs, self.filter_loops, self.human, self.diag, self.summary, self.extended, self.use_saved, self.font, self.report_font, self.report_wrap, self.dark
        );
        let _ = std::fs::write(p, t);
    }
    fn same(&self, o: &Settings) -> bool {
        self.filter_short == o.filter_short
            && self.short_secs == o.short_secs
            && self.filter_loops == o.filter_loops
            && self.human == o.human
            && self.diag == o.diag
            && self.summary == o.summary
            && self.extended == o.extended
            && self.use_saved == o.use_saved
            && self.font == o.font
            && self.report_font == o.report_font
            && self.report_wrap == o.report_wrap
            && self.dark == o.dark
    }
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    if dark {
        v.panel_fill = Color32::from_gray(30);
        v.selection.bg_fill = Color32::from_rgb(48, 74, 112);
        v.selection.stroke = egui::Stroke::new(1.0, Color32::from_rgb(150, 190, 240));
        v.widgets.inactive.bg_fill = Color32::from_gray(52);
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_gray(110));
        v.widgets.noninteractive.fg_stroke.color = Color32::from_gray(225);
        v.widgets.inactive.fg_stroke.color = Color32::from_gray(225);
    } else {
        v.panel_fill = Color32::from_gray(240);
        v.selection.bg_fill = Color32::from_rgb(205, 222, 245);
        v.selection.stroke = egui::Stroke::new(1.0, Color32::from_rgb(40, 90, 160));
        v.widgets.inactive.bg_fill = Color32::WHITE;
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_gray(120));
        v.widgets.noninteractive.fg_stroke.color = Color32::from_gray(20);
        v.widgets.inactive.fg_stroke.color = Color32::from_gray(20);
    }
    ctx.set_visuals(v);
}

fn box_fill(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_gray(22)
    } else {
        Color32::WHITE
    }
}
fn box_edge(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_gray(95)
    } else {
        Color32::from_gray(130)
    }
}
fn track_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_gray(44)
    } else {
        Color32::from_gray(236)
    }
}

/// Set every text size (and the widget sizes that follow from it) to `size` points.
fn apply_style(ctx: &egui::Context, size: f32) {
    use egui::TextStyle;
    ctx.global_style_mut(|st| {
        for (t, k) in [
            (TextStyle::Body, 1.0),
            (TextStyle::Button, 1.0),
            (TextStyle::Monospace, 1.0),
            (TextStyle::Small, 0.85),
            (TextStyle::Heading, 1.3),
        ] {
            if let Some(f) = st.text_styles.get_mut(&t) {
                f.size = size * k;
            }
        }
        st.spacing.icon_width = size * 1.1;
        st.spacing.interact_size.y = (size + 6.0).max(18.0);
    });
}

/// One open chart window.
struct ChartWin {
    id: u32,
    ch: Arc<crate::chart::Chart>,
    view: crate::chart::View,
}

/// State of the "Custom Playlist" dialog (BDInfo's FormPlaylist).
struct CustomDlg {
    name: String,
    filter: bool,
    pl: Option<String>,
    clip: Option<usize>,
    /// (playlist name, clip index) of each stream file in the new playlist.
    target: Vec<(String, usize)>,
    tsel: Option<usize>,
}

impl Default for CustomDlg {
    fn default() -> Self {
        CustomDlg {
            name: String::new(),
            filter: true,
            pl: None,
            clip: None,
            target: Vec::new(),
            tsel: None,
        }
    }
}

struct App {
    sh: Sh,
    tx: Sender<Cmd>,
    cancel: Arc<AtomicBool>,
    src: String,
    rows: Arc<Vec<PlRow>>,
    rows_ver: u64,
    report_ver: u64,
    checked: HashSet<String>,
    sel: Option<String>,
    set: Settings,
    short_buf: String,
    saved: Settings,
    /// Settings as they were when the Settings window was opened (Cancel returns to them).
    snap: Settings,
    themed: bool,
    show_about: bool,
    logo: Option<egui::TextureHandle>,
    upd: Arc<Mutex<String>>,
    /// Text size the window is currently drawn with, and the one being tried in Settings.
    styled: f32,
    pending: f32,
    /// Share of the list area taken by each of the four boxes, and column widths (as fractions) of the three tables.
    wts: [f32; 4],
    cols: [Vec<f32>; 3],
    /// True once a disc has been scanned (the button then reads Rescan).
    opened: bool,
    /// Source the watcher thread checks for a removed disc, and its verdict.
    watch: Arc<Mutex<String>>,
    gone: Arc<AtomicBool>,
    show_settings: bool,
    show_custom: bool,
    cd: CustomDlg,
    custom_count: u32,
    /// Playlists in the open report, and the chart pickers (playlist, stream, angle, type).
    report_names: Vec<String>,
    csel: [usize; 4],
    want_chart: bool,
    chart_note: Option<String>,
    chart_ver: u64,
    chart_seq: u32,
    charts: Vec<ChartWin>,
    show_report: bool,
    report: String,
    want_report: bool,
    /// Stream file selected in the middle list (index into the selected playlist's clips).
    sel_clip: Option<usize>,
    #[cfg(all(feature = "preview", unix))]
    preview: Option<crate::preview::Preview>,
    play_err: Option<String>,
    scan_msg: Option<String>,
}

/// The folder that holds BDMV (or the image/device itself), which is what libbluray and mpv open.
fn bd_root(src: &str) -> String {
    let p = std::path::Path::new(src.trim());
    if p.join("BDMV").is_dir() {
        p.display().to_string()
    } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("BDMV")) {
        p.parent().map_or(src.to_string(), |q| q.display().to_string())
    } else {
        src.trim().to_string()
    }
}

impl App {
    /// Play a playlist (or one of its stream files) in the preview window.
    #[cfg(all(feature = "preview", unix))]
    fn play_playlist(&mut self, clip: Option<usize>) {
        let Some(p) = self.rows.iter().find(|p| Some(&p.name) == self.sel.as_ref()) else {
            return;
        };
        let (mut start, mut length, mut title) = (0.0, None, p.name.clone());
        if let Some(c) = clip.and_then(|i| p.clips.get(i)) {
            start = p.clips[..clip.unwrap()].iter().map(|c| c.len).sum();
            length = Some(c.len);
            title = format!("{} / {}", p.name, c.name);
        }
        let t = crate::mpv::Target::Playlist {
            device: bd_root(&self.src),
            name: p.name.clone(),
            start,
            length,
            #[cfg(feature = "bluray")]
            segs: match clip {
                Some(i) => p.clips.get(i).and_then(|c| c.seg.clone()).into_iter().collect(),
                None => p.clips.iter().filter_map(|c| c.seg.clone()).collect(),
            },
            #[cfg(feature = "bluray")]
            bd: self.sh.lock().unwrap().bd.clone(),
        };
        self.start_preview(title, t);
    }
    #[cfg(all(feature = "preview", unix))]
    fn start_preview(&mut self, title: String, t: crate::mpv::Target) {
        if self.preview.is_none() {
            match crate::preview::Preview::new() {
                Ok(p) => self.preview = Some(p),
                Err(e) => {
                    self.play_err = Some(e);
                    return;
                }
            }
        }
        self.play_err = None;
        if let Some(p) = self.preview.as_mut() {
            p.play(title, t);
        }
    }
    #[cfg(not(all(feature = "preview", unix)))]
    fn play_playlist(&mut self, _clip: Option<usize>) {
        self.play_err = Some("This build has no preview support.".into());
    }
    /// The disc was taken out: clear the lists, log and report, stop the preview, release the drive, and
    /// turn the Rescan button back into Scan.
    fn disc_gone(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Close);
        #[cfg(all(feature = "preview", unix))]
        if let Some(p) = self.preview.as_mut() {
            p.stop();
        }
        self.opened = false;
        self.checked.clear();
        self.sel = None;
        self.sel_clip = None;
        self.report.clear();
        self.show_report = false;
        self.want_report = false;
        self.show_custom = false;
        self.cd = CustomDlg::default();
        self.custom_count = 0;
        self.report_names.clear();
        self.charts.clear();
        self.want_chart = false;
        self.chart_note = None;
        self.play_err = None;
        self.scan_msg = None;
        *self.watch.lock().unwrap() = String::new();
        // a drive keeps its name for the next disc; a mounted folder or image path is stale
        if !self.src.starts_with("/dev/") {
            self.src.clear();
        }
    }
    fn visible(&self) -> Vec<&PlRow> {
        self.rows
            .iter()
            .filter(|p| !(self.set.filter_short && p.len < self.set.short_secs as f64) && !(self.set.filter_loops && p.loops))
            .collect()
    }
    /// Checked playlists, or every visible one when none are checked.
    fn targets(&self) -> Vec<String> {
        let v = self.visible();
        let c: Vec<String> = v
            .iter()
            .filter(|p| self.checked.contains(&p.name))
            .map(|p| p.name.clone())
            .collect();
        if c.is_empty() {
            v.iter().map(|p| p.name.clone()).collect()
        } else {
            c
        }
    }
    fn open(&mut self, path: String) {
        let path = path.trim().to_string();
        if path.is_empty() {
            return;
        }
        self.src = path.clone();
        *self.watch.lock().unwrap() = path.clone();
        self.opened = true;
        self.checked.clear();
        self.sel = None;
        self.sh.lock().unwrap().busy = true;
        let _ = self.tx.send(Cmd::Open {
            path,
            saved: self.set.use_saved,
            extended: self.set.extended,
        });
    }
}

fn header(h: &mut egui_extras::TableRow, t: &str, right: bool) {
    h.col(|ui| {
        if right {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(t));
        } else {
            ui.label(t);
        }
    });
}

const GRIP: f32 = 7.0;
/// Gap between table columns; the separator lines sit in the middle of it.
const COLSP: f32 = 8.0;

/// A thin bar between two areas; returns how far it was dragged vertically this frame.
fn grip(ui: &mut egui::Ui) -> f32 {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), GRIP), egui::Sense::drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeVertical);
    let d = ui.visuals().dark_mode;
    let c = if resp.hovered() || resp.dragged() {
        Color32::from_gray(if d { 170 } else { 110 })
    } else {
        Color32::from_gray(if d { 80 } else { 185 })
    };
    ui.painter()
        .rect_filled(egui::Rect::from_center_size(r.center(), egui::vec2(48.0, 3.0)), 1.5, c);
    if resp.dragged() {
        resp.drag_delta().y
    } else {
        0.0
    }
}

/// Column separators for a table drawn into the current box: painted lines you can drag.
fn col_grips(ui: &mut egui::Ui, table: usize, widths: &[f32], moves: &mut Vec<(usize, usize, f32)>) {
    let r = ui.max_rect();
    let sp = COLSP;
    let mut x = r.left();
    for (j, w) in widths.iter().enumerate().take(widths.len() - 1) {
        x += w + sp;
        let line_x = x - sp / 2.0;
        let hit = egui::Rect::from_min_max(egui::pos2(line_x - 3.0, r.top()), egui::pos2(line_x + 3.0, r.bottom()));
        let resp = ui
            .interact(hit, ui.auto_id_with(("colgrip", table, j)), egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let d = ui.visuals().dark_mode;
        let c = if resp.hovered() || resp.dragged() {
            Color32::from_gray(if d { 170 } else { 90 })
        } else {
            Color32::from_gray(if d { 80 } else { 190 })
        };
        ui.painter().vline(line_x, r.top()..=r.bottom(), egui::Stroke::new(1.0, c));
        if resp.dragged() {
            moves.push((table, j, resp.drag_delta().x));
        }
    }
}

/// White list box with a thin border and a fixed height, like a WinForms ListView.
fn boxed(ui: &mut egui::Ui, h: f32, f: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    egui::Frame::new()
        .fill(box_fill(ui))
        .stroke(egui::Stroke::new(1.0, box_edge(ui)))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(COLSP, 0.0);
            ui.spacing_mut().scroll = egui::style::ScrollStyle {
                bar_width: 13.0,
                foreground_color: true,
                ..egui::style::ScrollStyle::solid()
            };
            let t = track_color(ui);
            ui.visuals_mut().extreme_bg_color = t; // scroll bar track
            ui.style_mut().interaction.selectable_labels = false; // so a click on a name selects the row
            ui.set_width(w - 2.0);
            ui.set_min_height(h);
            ui.set_max_height(h);
            f(ui);
        });
}

/// Fixed-height table. `widths` are the pixel widths of all columns; the last one takes whatever is left.
fn grid<'a>(ui: &'a mut egui::Ui, id: &str, h: f32, widths: &[f32]) -> TableBuilder<'a> {
    let mut t = TableBuilder::new(ui)
        .id_salt(id)
        .sense(egui::Sense::click())
        .auto_shrink([false, false])
        .min_scrolled_height(0.0)
        .max_scroll_height(h - 2.0)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .resizable(false);
    for &w in &widths[..widths.len() - 1] {
        t = t.column(Column::exact(w.max(10.0)).clip(true));
    }
    t.column(Column::remainder().clip(true))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let k = self.set.font / 14.0;
        if self.gone.swap(false, Ordering::Relaxed) && self.opened {
            self.disc_gone();
        }
        if self.themed != self.set.dark {
            apply_theme(&ctx, self.set.dark);
            self.themed = self.set.dark;
        }
        if self.styled != self.set.font {
            // text size changed (or first frame): restyle, and make the window big enough for it
            apply_style(&ctx, self.set.font);
            self.styled = self.set.font;
            let mon = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0)) * 0.95;
            let cur = ui.max_rect().size();
            let want = egui::vec2(790.0 * k, 840.0 * k).min(mon);
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(560.0 * k, 460.0 * k).min(mon)));
            if cur.x < want.x || cur.y < want.y {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(cur.max(want)));
            }
        }
        if !self.set.same(&self.saved) {
            self.set.save();
            self.saved = self.set.clone();
        }
        // pull shared state
        let (busy, label, done, total, elapsed, remaining, log) = {
            let mut u = self.sh.lock().unwrap();
            if let Some(m) = u.scan_done.take() {
                self.scan_msg = Some(m);
            }
            if let Some(n) = u.custom_added.take() {
                self.custom_count += 1;
                self.checked.insert(n);
            }
            if u.chart_ver != self.chart_ver {
                self.chart_ver = u.chart_ver;
                self.chart_note = u.chart_note.clone();
                if self.want_chart {
                    self.want_chart = false;
                    if let (None, Some(c)) = (&self.chart_note, u.chart.clone()) {
                        self.chart_seq += 1;
                        self.charts.push(ChartWin {
                            id: self.chart_seq,
                            view: crate::chart::View::new(&c),
                            ch: c,
                        });
                    }
                }
            }
            if u.rows_ver != self.rows_ver {
                self.rows = u.rows.clone();
                self.rows_ver = u.rows_ver;
            }
            if u.report_ver != self.report_ver {
                self.report_ver = u.report_ver;
                if self.want_report {
                    self.report = u.report.clone();
                    self.show_report = true;
                    self.want_report = false;
                }
            }
            (u.busy, u.label.clone(), u.done, u.total, u.elapsed, u.remaining, u.log.join("\n"))
        };
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped {
            self.open(p.display().to_string());
        }
        if self.sel.is_none() || !self.visible().iter().any(|p| Some(&p.name) == self.sel.as_ref()) {
            self.sel = self.visible().first().map(|p| p.name.clone());
        }
        let scanning = busy && (label.starts_with("Scanning") || label.starts_with("Identifying"));
        let loaded = !self.rows.is_empty();

        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
            let (font, bh, th, pbh) = (self.set.font, self.set.font + 13.0, self.set.font + 5.0, self.set.font + 9.0);
            let bsz = egui::vec2(76.0 * k, bh);
            ui.label("Select the Source BD-ROM:");
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let w = ui.available_width() - 3.0 * (bsz.x + gap);
                let r = ui.add_sized(
                    [w, bh - 2.0],
                    egui::TextEdit::singleline(&mut self.src)
                        .hint_text("Disc drive, folder or ISO image")
                        .align(egui::Align2::LEFT_CENTER),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.open(self.src.clone());
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Folder...").min_size(bsz))
                    .on_hover_text("Choose a BDMV folder")
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new().set_title("Select the BDMV folder").pick_folder() {
                        self.open(p.display().to_string());
                    }
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("ISO...").min_size(bsz))
                    .on_hover_text("Choose a disc image (.iso)")
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Disc image", &["iso", "img", "udf"]).pick_file() {
                        self.open(p.display().to_string());
                    }
                }
                let again = if self.opened { "Rescan" } else { "Scan" };
                if ui.add_enabled(!busy, egui::Button::new(again).min_size(bsz)).clicked() {
                    let p = if self.src.trim().is_empty() {
                        default_source().unwrap_or_default()
                    } else {
                        self.src.clone()
                    };
                    self.open(p);
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("Playlists:");
                let b = |t: &str| egui::Button::new(t).min_size(bsz);
                if ui.add_enabled(!busy && loaded, b("Select All")).clicked() {
                    let all: Vec<String> = self.visible().iter().map(|p| p.name.clone()).collect();
                    self.checked.extend(all);
                }
                if ui.add_enabled(!busy && loaded, b("Unselect All")).clicked() {
                    self.checked.clear();
                }
                if ui.add_enabled(!busy && loaded, b("Custom...")).clicked() {
                    self.show_custom = true;
                    self.cd = CustomDlg {
                        name: format!("USER.{:03}", self.custom_count + 1),
                        ..Default::default()
                    };
                }
                // preview buttons sit together on the right (right-to-left, so listed last-first)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    #[cfg(all(feature = "preview", unix))]
                    if ui
                        .add(b("Play File..."))
                        .on_hover_text("Choose a .m2ts, .mkv, .mp4 ... file and play it in the preview window")
                        .clicked()
                    {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("Video", &["m2ts", "mts", "ts", "mkv", "mp4", "m4v", "avi"])
                            .pick_file()
                        {
                            let t = p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                            self.start_preview(t, crate::mpv::Target::File(p.display().to_string()));
                        }
                    }
                    if ui
                        .add_enabled(loaded && self.sel_clip.is_some(), b("Play Clip"))
                        .on_hover_text("Play the selected stream file (double-click a stream file does the same)")
                        .clicked()
                    {
                        self.play_playlist(self.sel_clip);
                    }
                    if ui
                        .add_enabled(loaded && self.sel.is_some(), b("Play List"))
                        .on_hover_text("Play the selected playlist in the preview window")
                        .clicked()
                    {
                        self.play_playlist(None);
                    }
                });
            });

            // The four areas share the window height; the grips between them can be dragged.
            let rows = self.rows.clone();
            let vis: Vec<&PlRow> = self.visible();
            let human = self.set.human;
            let mut clicked: Option<String> = None;
            let mut play_pl = false;
            let mut clip_click: Option<usize> = None;
            let mut clip_play = false;
            let mut toggled: Vec<(String, bool)> = Vec::new();
            let mut col_moves: Vec<(usize, usize, f32)> = Vec::new();
            let right = egui::Layout::right_to_left(egui::Align::Center);
            let rh = font + 5.0;
            let hh = rh + 6.0;
            let sy = ui.spacing().item_spacing.y;
            let min_h = hh + rh + 6.0;
            let cap = font + 2.0;
            let flex = (ui.available_height() - 3.0 * GRIP - 8.0 - 11.0 * sy - 2.0 * cap - th - pbh - bh - 4.0).max(4.0 * min_h);
            let hs: [f32; 4] = std::array::from_fn(|i| (self.wts[i] * flex).max(min_h));
            let minf = min_h / flex;
            let mut grip_moves: [f32; 3] = [0.0; 3];
            let cw = [self.cols[0].clone(), self.cols[1].clone(), self.cols[2].clone()];

            // playlists
            boxed(ui, hs[0], |ui| {
                let tw = ui.available_width();
                let w: Vec<f32> = cw[0].iter().map(|f| f * tw).collect();
                grid(ui, "playlists", hs[0], &w)
                    .header(hh, |mut h| {
                        header(&mut h, "Playlist File", false);
                        header(&mut h, "Group", false);
                        header(&mut h, "Length", true);
                        header(&mut h, if human { "Estimated Size" } else { "Estimated Bytes" }, true);
                        header(&mut h, if human { "Measured Size" } else { "Measured Bytes" }, true);
                    })
                    .body(|body| {
                        body.rows(rh, vis.len(), |mut row| {
                            let p = vis[row.index()];
                            let mut on = self.checked.contains(&p.name);
                            row.set_selected(Some(&p.name) == self.sel.as_ref());
                            row.col(|ui| {
                                if ui.checkbox(&mut on, &p.name).changed() {
                                    toggled.push((p.name.clone(), on));
                                }
                            });
                            row.col(|ui| {
                                ui.label(p.group.to_string());
                            });
                            row.col(|ui| {
                                ui.with_layout(right, |ui| ui.label(hms(p.len)));
                            });
                            row.col(|ui| {
                                ui.with_layout(right, |ui| ui.label(num(p.est, human)));
                            });
                            row.col(|ui| {
                                ui.with_layout(right, |ui| ui.label(num(p.meas, human)));
                            });
                            if row.response().clicked() {
                                clicked = Some(p.name.clone());
                            }
                            if row.response().double_clicked() {
                                play_pl = true;
                            }
                        });
                    });
                col_grips(ui, 0, &w, &mut col_moves);
            });
            grip_moves[0] = grip(ui);
            for (n, on) in toggled {
                clicked = Some(n.clone());
                if on {
                    self.checked.insert(n);
                } else {
                    self.checked.remove(&n);
                }
            }
            if clicked.is_some() {
                if clicked != self.sel {
                    self.sel_clip = None;
                }
                self.sel = clicked;
            }
            if play_pl {
                self.play_playlist(None);
            }
            let cur = rows.iter().find(|p| Some(&p.name) == self.sel.as_ref());

            // stream files
            let pl = self.sel.clone().unwrap_or_default();
            ui.add_sized(
                [ui.available_width(), cap],
                egui::Label::new(RichText::new(format!("Stream files in {pl}")).strong()),
            );
            boxed(ui, hs[1], |ui| {
                let tw = ui.available_width();
                let w: Vec<f32> = cw[1].iter().map(|f| f * tw).collect();
                grid(ui, "clips", hs[1], &w)
                    .header(hh, |mut h| {
                        header(&mut h, "Stream File", false);
                        header(&mut h, "Index", false);
                        header(&mut h, "Length", true);
                        header(&mut h, if human { "Estimated Size" } else { "Estimated Bytes" }, true);
                        header(&mut h, if human { "Measured Size" } else { "Measured Bytes" }, true);
                    })
                    .body(|body| {
                        let clips = cur.map_or(&[][..], |p| &p.clips[..]);
                        body.rows(rh, clips.len(), |mut row| {
                            let c = &clips[row.index()];
                            row.set_selected(Some(row.index()) == self.sel_clip);
                            row.col(|ui| {
                                ui.label(&c.name);
                            });
                            row.col(|ui| {
                                ui.label(c.index.to_string());
                            });
                            row.col(|ui| {
                                ui.with_layout(right, |ui| ui.label(hms(c.len)));
                            });
                            row.col(|ui| {
                                ui.with_layout(right, |ui| ui.label(num(c.est, human)));
                            });
                            row.col(|ui| {
                                ui.with_layout(right, |ui| ui.label(num(c.meas, human)));
                            });
                            if row.response().clicked() {
                                clip_click = Some(row.index());
                            }
                            if row.response().double_clicked() {
                                clip_play = true;
                            }
                        });
                    });
                col_grips(ui, 1, &w, &mut col_moves);
            });
            if clip_click.is_some() {
                self.sel_clip = clip_click;
            }
            if clip_play {
                self.play_playlist(self.sel_clip);
            }
            grip_moves[1] = grip(ui);

            // streams
            ui.add_sized(
                [ui.available_width(), cap],
                egui::Label::new(RichText::new(format!("Streams in {pl}")).strong()),
            );
            boxed(ui, hs[2], |ui| {
                let tw = ui.available_width();
                let w: Vec<f32> = cw[2].iter().map(|f| f * tw).collect();
                grid(ui, "streams", hs[2], &w)
                    .header(hh, |mut h| {
                        header(&mut h, "Codec", false);
                        header(&mut h, "Language", false);
                        header(&mut h, "Bit Rate", false);
                        header(&mut h, "Description", false);
                    })
                    .body(|body| {
                        let st = cur.map_or(&[][..], |p| &p.streams[..]);
                        body.rows(rh, st.len(), |mut row| {
                            let s = &st[row.index()];
                            row.col(|ui| {
                                ui.label(&s.codec);
                            });
                            row.col(|ui| {
                                ui.label(&s.lang);
                            });
                            row.col(|ui| {
                                ui.label(&s.rate);
                            });
                            row.col(|ui| {
                                ui.label(&s.desc);
                            });
                        });
                    });
                col_grips(ui, 2, &w, &mut col_moves);
            });
            grip_moves[2] = grip(ui);

            // log
            boxed(ui, hs[3], |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("log")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(&log).wrap());
                        });
                    });
            });

            // apply grip and column drags
            for (i, dy) in grip_moves.into_iter().enumerate() {
                if dy != 0.0 {
                    let tot = self.wts[i] + self.wts[i + 1];
                    let a = (self.wts[i] + dy / flex).clamp(minf, (tot - minf).max(minf));
                    (self.wts[i], self.wts[i + 1]) = (a, tot - a);
                }
            }
            for (t, c, dx) in col_moves {
                let tw = ui.available_width().max(100.0);
                let f = &mut self.cols[t];
                let tot = f[c] + f[c + 1];
                let a = (f[c] + dx / tw).clamp(0.04, (tot - 0.04).max(0.04));
                (f[c], f[c + 1]) = (a, tot - a);
            }

            ui.allocate_ui_with_layout([ui.available_width(), th].into(), right, |ui| {
                ui.label(format!("{}  /  {}", hms(remaining), hms(elapsed)));
                ui.label("Time Remaining / Elapsed:");
            });
            let frac = if total > 0 {
                (done as f32 / total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let text = if !busy {
                String::new()
            } else if total == 0 {
                label.clone()
            } else {
                format!("{label}  {:.0}%", frac * 100.0)
            };
            let pr = ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(ui.available_width())
                    .desired_height(pbh)
                    .corner_radius(2.0)
                    .fill(if frac > 0.0 {
                        Color32::from_rgb(6, 176, 37)
                    } else {
                        Color32::TRANSPARENT
                    }),
            );
            ui.painter().text(
                pr.rect.left_center() + egui::vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::TextStyle::Body.resolve(ui.style()),
                ui.visuals().text_color(),
            );
            ui.add_space(2.0);
            let bs = egui::vec2(130.0 * k, bh);
            ui.horizontal(|ui| {
                let gap = (ui.available_width() - 4.0 * bs.x) / 3.0 - 2.0 * ui.spacing().item_spacing.x;
                if scanning {
                    if ui.add(egui::Button::new("Cancel Scan").min_size(bs)).clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                } else if ui
                    .add_enabled(!busy && loaded, egui::Button::new("Scan Bitrates").min_size(bs))
                    .on_hover_text("Reads the whole video files to measure each stream's real bit rate (can take several minutes)")
                    .clicked()
                {
                    self.sh.lock().unwrap().busy = true;
                    let _ = self.tx.send(Cmd::Scan {
                        names: self.targets(),
                        extended: self.set.extended,
                        save: self.set.use_saved,
                    });
                }
                ui.add_space(gap);
                if ui
                    .add_enabled(!busy && loaded, egui::Button::new("View Report...").min_size(bs))
                    .clicked()
                {
                    self.want_report = true;
                    self.report_names = self.targets();
                    self.csel = [0; 4];
                    self.chart_note = None;
                    let _ = self.tx.send(Cmd::Report {
                        names: self.targets(),
                        diag: self.set.diag,
                        summary: self.set.summary,
                    });
                }
                ui.add_space(gap);
                if ui.add(egui::Button::new("Settings...").min_size(bs)).clicked() {
                    self.show_settings = true;
                    self.pending = self.set.font;
                    self.snap = self.set.clone();
                    self.short_buf = format!("{}", self.set.short_secs as u32);
                }
                ui.add_space(gap);
                if ui.add(egui::Button::new("About...").min_size(bs)).clicked() {
                    self.show_about = true;
                }
            });
        });

        // dialogs
        #[cfg(all(feature = "preview", unix))]
        if let Some(p) = self.preview.as_mut() {
            p.show(&ctx);
        }
        if self.scan_msg.is_some() {
            let m = self.scan_msg.clone().unwrap_or_default();
            let mut close = false;
            if !os_window(&ctx, "scan_done", "Scan complete", [460.0, 190.0], |ui| {
                let (main, hint) = m.split_once("\n\n").unwrap_or((m.as_str(), ""));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                    let green = Color32::from_rgb(46, 160, 67);
                    let p = ui.painter();
                    p.circle_filled(r.center(), 22.0, green);
                    let c = r.center();
                    p.add(egui::Shape::line(
                        vec![c + egui::vec2(-9.0, 1.0), c + egui::vec2(-2.5, 8.0), c + egui::vec2(10.0, -8.0)],
                        egui::Stroke::new(4.0, Color32::WHITE),
                    ));
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Scan complete").strong().size(20.0));
                        ui.add(egui::Label::new(main).wrap());
                        if !hint.is_empty() {
                            ui.add(egui::Label::new(RichText::new(hint).weak()).wrap());
                        }
                    });
                });
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ok = ui.add(egui::Button::new("OK").min_size(egui::vec2(96.0, 30.0)));
                    if ok.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            }) || close
            {
                self.scan_msg = None;
            }
        }
        if let Some(e) = self.play_err.clone() {
            if !os_window(&ctx, "preview_err", "Preview", [460.0, 160.0], |ui| {
                ui.add(egui::Label::new(e.as_str()).wrap());
            }) {
                self.play_err = None;
            }
        }

        let mut open = self.show_settings;
        if open {
            let (mut ok, mut cancel) = (false, false);
            open = os_window(&ctx, "settings", "Settings", [560.0, 600.0], |ui| {
                ui.checkbox(&mut self.set.filter_short, "Filter out short playlists");
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.add_enabled_ui(self.set.filter_short, |ui| {
                        ui.label("Hide playlists shorter than");
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.short_buf)
                                .desired_width(56.0)
                                .align(egui::Align2::CENTER_CENTER),
                        );
                        if r.changed() {
                            if let Ok(v) = self.short_buf.trim().parse::<u32>() {
                                self.set.short_secs = v.min(36000) as f32;
                            }
                        }
                        ui.label("seconds");
                    });
                });
                ui.checkbox(&mut self.set.filter_loops, "Filter out looping playlists");
                ui.checkbox(&mut self.set.human, "Show sizes in human readable format (GB / MB)");
                ui.checkbox(&mut self.set.diag, "Generate stream diagnostics");
                ui.checkbox(&mut self.set.summary, "Generate text summary");
                ui.checkbox(&mut self.set.extended, "Extended HEVC diagnostics (wait for HDR metadata)");
                ui.separator();
                ui.label(RichText::new("Saved bitrate scans").strong());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.set.use_saved, "Save bitrate scans and reuse them for the same disc");
                    ui.label(RichText::new("(i)").weak())
                        .on_hover_cursor(egui::CursorIcon::Help)
                        .on_hover_text(
                            "After a bitrate scan finishes, TuxInfo saves the measurements for that disc in the folder below.\n\
                     When you open the same disc again (from the drive, a folder or an ISO), they are loaded automatically,\n\
                     so the report and charts are ready without scanning the disc again. The log tells you when this happens.\n\n\
                     Scan Bitrates always measures again and replaces the saved copy. A saved scan is used only if the disc matches\n\
                     and the Extended HEVC diagnostics setting is the same as when it was saved.",
                        );
                });
                if let Some(dir) = crate::scancache::dir() {
                    let (n, bytes) = crate::scancache::usage();
                    let used = if n == 0 {
                        "none saved".to_string()
                    } else {
                        format!("{n} saved, {}", num(bytes, true))
                    };
                    ui.add(egui::Label::new(RichText::new(format!("{}  ({used})", dir.display())).weak()).wrap());
                    ui.horizontal(|ui| {
                        if ui.button("Open folder").clicked() {
                            let _ = std::fs::create_dir_all(&dir);
                            #[cfg(windows)]
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                            #[cfg(not(windows))]
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                        }
                        if ui.add_enabled(n > 0, egui::Button::new("Delete all saved scans")).clicked() {
                            crate::scancache::clear();
                        }
                    });
                }
                ui.separator();
                ui.label(RichText::new("Appearance").strong());
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.set.dark, false, "Light");
                    ui.radio_value(&mut self.set.dark, true, "Dark");
                });
                ui.separator();
                ui.label(RichText::new("Text size").strong());
                ui.horizontal(|ui| {
                    ui.label("Size:");
                    ui.add(
                        egui::Slider::new(&mut self.pending, 9.0..=40.0)
                            .suffix(" pt")
                            .step_by(1.0)
                            .fixed_decimals(0),
                    );
                    if ui.button("Default").clicked() {
                        self.pending = 16.0;
                    }
                });
                ui.label("Sample (the window changes when you press OK):");
                egui::Frame::new()
                    .fill(box_fill(ui))
                    .stroke(egui::Stroke::new(1.0, box_edge(ui)))
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(400.0);
                        for t in [
                            "00800.MPLS    01:59:13    35,965,906,944",
                            "MPEG-4 AVC Video    1080p / 23.976 fps / 16:9",
                            "DTS-HD High-Res Audio    English    5.1 / 48 kHz",
                        ] {
                            ui.add(egui::Label::new(RichText::new(t).size(self.pending).color(ui.visuals().text_color())).wrap());
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ok = ui.add(egui::Button::new("OK").min_size(egui::vec2(90.0, 0.0))).clicked();
                    cancel = ui.add(egui::Button::new("Cancel").min_size(egui::vec2(90.0, 0.0))).clicked();
                });
                ui.add(
                    egui::Label::new(RichText::new("OK keeps your changes. Cancel, or closing this window, undoes them.").weak()).wrap(),
                );
            });
            if ok {
                self.set.font = self.pending;
                open = false;
            } else if cancel || !open {
                self.set = self.snap.clone();
                self.pending = self.set.font;
                open = false;
            }
        }
        self.show_settings = open;

        if self.show_about {
            let upd = self.upd.clone();
            let mut check = false;
            let busy = upd.lock().unwrap().starts_with("Checking");
            let has_repo = !crate::app_info::REPO_URL.is_empty();
            let mut close = false;
            let logo = self
                .logo
                .get_or_insert_with(|| {
                    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../packaging/icons/tuxinfo-256.png")).unwrap_or_default();
                    let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
                    ctx.load_texture("tuxinfo-logo", img, egui::TextureOptions::LINEAR)
                })
                .clone();
            let open = os_window(&ctx, "about", "About TuxInfo", [540.0, 650.0], |ui| {
                let card = |ui: &mut egui::Ui, title: &str, f: &mut dyn FnMut(&mut egui::Ui)| {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .stroke(egui::Stroke::new(1.0, box_edge(ui)))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(title).strong().size(15.0));
                            ui.add_space(4.0);
                            f(ui);
                        });
                    ui.add_space(8.0);
                };
                ui.spacing_mut().item_spacing.y = 6.0;
                const LOGO: f32 = 104.0;
                const PAD: f32 = 16.0;
                ui.horizontal_top(|ui| {
                    let text_w = (ui.available_width() - LOGO - PAD - ui.spacing().item_spacing.x * 2.0).max(0.0);
                    ui.allocate_ui_with_layout(egui::vec2(text_w, 0.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        ui.label(RichText::new("TuxInfo").strong().size(32.0));
                        ui.label(RichText::new(format!("Version {}", crate::app_info::version())).size(15.0));
                        ui.label(RichText::new("Blu-ray and Ultra HD disc analyzer").weak());
                        ui.label(RichText::new(format!("by {}", crate::app_info::STUDIO)).weak());
                        ui.add_space(8.0);
                        if has_repo {
                            ui.hyperlink_to(crate::app_info::REPO_URL, crate::app_info::REPO_URL);
                        } else {
                            ui.label(RichText::new("The repository address will be added here.").weak());
                        }
                        ui.hyperlink_to(crate::app_info::SUPPORT_EMAIL, format!("mailto:{}", crate::app_info::SUPPORT_EMAIL));
                    });
                    ui.vertical(|ui| {
                        ui.add_space(PAD);
                        ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(LOGO, LOGO)));
                    });
                });
                ui.add_space(10.0);
                card(ui, "About", &mut |ui| {
                    ui.add(egui::Label::new("TuxInfo is based on BDInfo, the Blu-ray disc analyzer by Cinema Squid, continued by the UniqProject developers.").wrap());
                    ui.hyperlink_to("github.com/UniqProject/BDInfo", "https://github.com/UniqProject/BDInfo");
                });
                card(ui, "License", &mut |ui| {
                    ui.add(
                        egui::Label::new(
                            "BDInfo is licensed under the LGPL-2.1. TuxInfo is a derivative work and is released under the same license.",
                        )
                        .wrap(),
                    );
                });
                card(ui, "Built with", &mut |ui| {
                    egui::Grid::new("about_libs").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                        for (n, d) in [
                            ("libbluray + libmmbd", "reads encrypted discs"),
                            ("libmpv (FFmpeg)", "plays the preview"),
                            ("egui", "draws this window"),
                        ] {
                            ui.label(RichText::new(n).strong());
                            ui.label(RichText::new(d).weak());
                            ui.end_row();
                        }
                    });
                });
                card(ui, "Updates", &mut |ui| {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(has_repo && !busy, egui::Button::new("Check for updates")).clicked() {
                            check = true;
                        }
                        let msg = upd.lock().unwrap().clone();
                        if !has_repo {
                            ui.label(RichText::new("Available once the repository address is set.").weak());
                        } else {
                            ui.add(egui::Label::new(msg).wrap());
                        }
                    });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Close").min_size(egui::vec2(96.0, 30.0))).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        close = true;
                    }
                });
            });
            self.show_about = open && !close;
            if check {
                *upd.lock().unwrap() = "Checking...".into();
                let ctx2 = ctx.clone();
                std::thread::spawn(move || {
                    let m = match crate::app_info::check() {
                        Ok(crate::app_info::Update::Current) => format!("You have the latest version ({}).", crate::app_info::version()),
                        Ok(crate::app_info::Update::Newer(t)) => format!(
                            "Version {t} is available: {}/releases",
                            crate::app_info::REPO_URL.trim_end_matches('/')
                        ),
                        Err(e) => e,
                    };
                    *upd.lock().unwrap() = m;
                    ctx2.request_repaint();
                });
            }
        }

        if self.show_custom {
            let rows = self.rows.clone();
            let mut cd = std::mem::take(&mut self.cd);
            let human = self.set.human;
            let (mut ok, mut cancel) = (false, false);
            let keep = os_window(&ctx, "custom_playlist", "Custom Playlist", [940.0, 600.0], |ui| {
                ui.horizontal(|ui| {
                    ui.label("Playlist name:");
                    ui.add(egui::TextEdit::singleline(&mut cd.name).desired_width(160.0));
                    ui.checkbox(&mut cd.filter, "Filter out incompatible playlists");
                });
                ui.separator();
                let find = |n: &str| rows.iter().find(|r| r.name == n);
                let compatible = |p: &PlRow, target: &[(String, usize)]| {
                    target.iter().filter_map(|(n, i)| find(n).and_then(|r| r.clips.get(*i))).all(|t| {
                        p.clips
                            .iter()
                            .all(|c| c.types.iter().all(|(pid, ty)| t.types.iter().all(|(p2, t2)| pid != p2 || ty == t2)))
                    })
                };
                let hh = (ui.available_height() - 80.0).max(120.0);
                ui.columns(2, |cols| {
                    let ui = &mut cols[0];
                    ui.strong("Playlists");
                    egui::ScrollArea::vertical()
                        .id_salt("cp_pl")
                        .max_height(hh * 0.5 - 24.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("cp_pl_grid").num_columns(3).spacing([14.0, 2.0]).show(ui, |ui| {
                                for r in rows.iter() {
                                    if cd.filter && !cd.target.is_empty() && !compatible(r, &cd.target) {
                                        continue;
                                    }
                                    let sel = cd.pl.as_deref() == Some(r.name.as_str());
                                    let mut hit = ui.selectable_label(sel, &r.name).clicked();
                                    hit |= ui.selectable_label(sel, hms(r.len)).clicked();
                                    hit |= ui.selectable_label(sel, num(r.est, human)).clicked();
                                    ui.end_row();
                                    if hit && !sel {
                                        cd.pl = Some(r.name.clone());
                                        cd.clip = None;
                                    }
                                }
                            });
                        });
                    ui.add_space(4.0);
                    ui.strong("Stream files of the selected playlist");
                    let cur = cd.pl.as_deref().and_then(find);
                    egui::ScrollArea::vertical()
                        .id_salt("cp_cl")
                        .max_height(hh * 0.5 - 56.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(r) = cur {
                                egui::Grid::new("cp_cl_grid").num_columns(3).spacing([14.0, 2.0]).show(ui, |ui| {
                                    for (i, c) in r.clips.iter().enumerate() {
                                        let sel = cd.clip == Some(i);
                                        let mut hit = ui.selectable_label(sel, &c.name).clicked();
                                        hit |= ui.selectable_label(sel, hms(c.len)).clicked();
                                        hit |= ui.selectable_label(sel, num(c.est, human)).clicked();
                                        ui.end_row();
                                        if hit {
                                            cd.clip = Some(i);
                                        }
                                    }
                                });
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(cur.is_some() && cd.clip.is_some(), egui::Button::new("Add >"))
                            .clicked()
                        {
                            if let (Some(r), Some(i)) = (cur, cd.clip) {
                                cd.target.push((r.name.clone(), i));
                            }
                        }
                        if ui.add_enabled(cur.is_some(), egui::Button::new("Add All >>")).clicked() {
                            if let Some(r) = cur {
                                cd.target.extend((0..r.clips.len()).map(|i| (r.name.clone(), i)));
                            }
                        }
                    });

                    let ui = &mut cols[1];
                    ui.strong("New playlist");
                    egui::ScrollArea::vertical()
                        .id_salt("cp_tg")
                        .max_height(hh - 24.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("cp_tg_grid").num_columns(3).spacing([14.0, 2.0]).show(ui, |ui| {
                                for (i, (pn, ci)) in cd.target.iter().enumerate() {
                                    let Some(c) = find(pn).and_then(|r| r.clips.get(*ci)) else {
                                        continue;
                                    };
                                    let sel = cd.tsel == Some(i);
                                    let mut hit = ui.selectable_label(sel, &c.name).clicked();
                                    hit |= ui.selectable_label(sel, hms(c.len)).clicked();
                                    hit |= ui.selectable_label(sel, num(c.est, human)).clicked();
                                    ui.end_row();
                                    if hit {
                                        cd.tsel = Some(i);
                                    }
                                }
                            });
                        });
                    ui.horizontal(|ui| {
                        let n = cd.target.len();
                        let s = cd.tsel.filter(|&i| i < n);
                        if ui.add_enabled(s.is_some(), egui::Button::new("< Remove")).clicked() {
                            if let Some(i) = s {
                                cd.target.remove(i);
                                cd.tsel = None;
                            }
                        }
                        if ui.add_enabled(s.is_some_and(|i| i > 0), egui::Button::new("Up")).clicked() {
                            if let Some(i) = s {
                                cd.target.swap(i, i - 1);
                                cd.tsel = Some(i - 1);
                            }
                        }
                        if ui.add_enabled(s.is_some_and(|i| i + 1 < n), egui::Button::new("Down")).clicked() {
                            if let Some(i) = s {
                                cd.target.swap(i, i + 1);
                                cd.tsel = Some(i + 1);
                            }
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!cd.target.is_empty() && !cd.name.trim().is_empty(), egui::Button::new("OK"))
                        .clicked()
                    {
                        ok = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if ok {
                let _ = self.tx.send(Cmd::Custom {
                    name: cd.name.trim().to_uppercase(),
                    items: cd.target.clone(),
                });
            }
            self.show_custom = keep && !ok && !cancel;
            self.cd = cd;
        }

        if self.show_report {
            // its own window, so it can be moved outside the program window (an embedded window where that is not supported)
            let (mut keep, mut rf, mut rw) = (true, self.set.report_font, self.set.report_wrap);
            let rows = self.rows.clone();
            let pls: Vec<&PlRow> = self
                .report_names
                .iter()
                .filter_map(|n| rows.iter().find(|r| &r.name == n))
                .collect();
            let mut cs = self.csel;
            let mut gen_chart: Option<(String, u16, usize, usize)> = None;
            let note = self.chart_note.clone();
            let (text, name) = (
                &self.report,
                format!(
                    "TuxInfo.{}.txt",
                    self.src
                        .trim_end_matches(['/', '\\'])
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("report")
                ),
            );
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("report"),
                egui::ViewportBuilder::default()
                    .with_title("TuxInfo Report")
                    .with_app_id("tuxinfo")
                    .with_inner_size([900.0, 640.0])
                    .with_min_inner_size([420.0, 260.0]),
                |ui, _class| {
                    if ui.ctx().input(|i| i.viewport().close_requested()) {
                        keep = false;
                    }
                    let (zoom, cmd) = ui.input(|i| (i.zoom_delta(), i.modifiers.command));
                    if cmd && (zoom - 1.0).abs() > 1e-3 {
                        rf = (rf * zoom).round().clamp(8.0, 48.0);
                    }
                    egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                        ui.spacing_mut().scroll = egui::style::ScrollStyle {
                            bar_width: 13.0,
                            foreground_color: true,
                            ..egui::style::ScrollStyle::solid()
                        };
                        let t = track_color(ui);
                        ui.visuals_mut().extreme_bg_color = t;
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                        ui.spacing_mut().button_padding = egui::vec2(10.0, 4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                            if ui.button("Save...").clicked() {
                                if let Some(p) = rfd::FileDialog::new().set_file_name(name.as_str()).save_file() {
                                    let _ = std::fs::write(p, text);
                                }
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.checkbox(&mut rw, "Wrap lines");
                                ui.add_space(8.0);
                                if ui.button("+").clicked() {
                                    rf = (rf + 1.0).min(48.0);
                                }
                                ui.label(format!("{rf:.0} pt"));
                                if ui.button("\u{2212}").clicked() {
                                    rf = (rf - 1.0).max(8.0);
                                }
                                ui.label("Text size");
                            });
                        });
                        if !pls.is_empty() {
                            egui::CollapsingHeader::new("Bitrate charts").default_open(true).show(ui, |ui| {
                                cs[0] = cs[0].min(pls.len() - 1);
                                let prev = cs[0];
                                let n_before = pls[cs[0]];
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    ui.label("Chart:");
                                    egui::ComboBox::from_id_salt("chart_pl")
                                        .width(96.0)
                                        .selected_text(n_before.name.as_str())
                                        .show_ui(ui, |ui| {
                                            for (i, p) in pls.iter().enumerate() {
                                                ui.selectable_value(&mut cs[0], i, p.name.as_str());
                                            }
                                        });
                                    if cs[0] != prev {
                                        cs[1] = 0;
                                        cs[2] = 0;
                                    }
                                    let p = pls[cs[0]];
                                    cs[1] = cs[1].min(p.vids.len().saturating_sub(1));
                                    cs[2] = cs[2].min(p.angles);
                                    egui::ComboBox::from_id_salt("chart_st")
                                        .width(190.0)
                                        .selected_text(p.vids.get(cs[1]).map_or("(no video)".to_string(), |v| v.1.replace(" Video", "")))
                                        .show_ui(ui, |ui| {
                                            for (i, v) in p.vids.iter().enumerate() {
                                                ui.selectable_value(&mut cs[1], i, v.1.replace(" Video", ""));
                                            }
                                        });
                                    ui.label("Angle:");
                                    egui::ComboBox::from_id_salt("chart_an")
                                        .width(36.0)
                                        .selected_text(cs[2].to_string())
                                        .show_ui(ui, |ui| {
                                            for a in 0..=p.angles {
                                                ui.selectable_value(&mut cs[2], a, a.to_string());
                                            }
                                        });
                                    egui::ComboBox::from_id_salt("chart_ty")
                                        .width(190.0)
                                        .selected_text(crate::chart::TYPES[cs[3].min(5)].trim_start_matches("Video "))
                                        .show_ui(ui, |ui| {
                                            for (i, t) in crate::chart::TYPES.iter().enumerate() {
                                                ui.selectable_value(&mut cs[3], i, t.trim_start_matches("Video "));
                                            }
                                        });
                                    if ui
                                        .add_enabled(!p.vids.is_empty(), egui::Button::new("Generate"))
                                        .on_hover_text("Open the chart in its own window")
                                        .clicked()
                                    {
                                        gen_chart = Some((p.name.clone(), p.vids[cs[1]].0, cs[2], cs[3]));
                                    }
                                });
                                if let Some(n) = &note {
                                    ui.colored_label(Color32::from_rgb(200, 60, 40), n.as_str());
                                }
                            });
                        }
                        ui.add_space(4.0);
                        egui::Frame::new()
                            .fill(box_fill(ui))
                            .stroke(egui::Stroke::new(1.0, box_edge(ui)))
                            .show(ui, |ui| {
                                let area = if rw {
                                    egui::ScrollArea::vertical()
                                } else {
                                    egui::ScrollArea::both()
                                };
                                area.auto_shrink([false, false]).show(ui, |ui| {
                                    let label = egui::Label::new(
                                        RichText::new(text.as_str()).monospace().size(rf).color(ui.visuals().text_color()),
                                    );
                                    ui.add(if rw { label.wrap() } else { label.extend() });
                                });
                            });
                    });
                },
            );
            self.set.report_font = rf;
            self.set.report_wrap = rw;
            self.show_report = keep;
            self.csel = cs;
            if let Some((name, pid, angle, kind)) = gen_chart {
                self.want_chart = true;
                self.chart_note = None;
                let _ = self.tx.send(Cmd::Chart { name, pid, angle, kind });
            }
        }

        // chart windows
        if !self.charts.is_empty() {
            let mut wins = std::mem::take(&mut self.charts);
            wins.retain_mut(|w| {
                let mut keep = true;
                ctx.show_viewport_immediate(
                    egui::ViewportId::from_hash_of(("chart", w.id)),
                    egui::ViewportBuilder::default()
                        .with_title(w.ch.window_title.as_str())
                        .with_app_id("tuxinfo")
                        .with_inner_size([960.0, 600.0])
                        .with_min_inner_size([420.0, 300.0]),
                    |ui, _class| {
                        if ui.ctx().input(|i| i.viewport().close_requested()) {
                            keep = false;
                        }
                        let mut save = false;
                        let mut status = String::new();
                        let mut shot_rect = egui::Rect::NOTHING;
                        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                save = ui.button("Save Image As...").clicked();
                                if matches!(w.ch.kind, crate::chart::Kind::Lines { .. }) {
                                    if ui.button("Reset zoom").clicked() {
                                        w.view = crate::chart::View::new(&w.ch);
                                    }
                                    ui.label("Mouse wheel zooms, drag pans, double-click resets.");
                                }
                            });
                            let h = ui.available_height() - 22.0;
                            ui.allocate_ui([ui.available_width(), h.max(60.0)].into(), |ui| {
                                let d = crate::chart::draw(ui, &w.ch, &mut w.view);
                                status = d.status;
                                shot_rect = d.rect;
                            });
                            ui.label(status);
                        });
                        if save {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name(w.ch.file.as_str())
                                .add_filter("PNG image", &["png"])
                                .save_file()
                            {
                                let ppp = ui.ctx().pixels_per_point();
                                ui.ctx().request_screenshot(move |img| {
                                    let [iw, ih] = img.size;
                                    let x0 = ((shot_rect.min.x * ppp).round().max(0.0) as usize).min(iw);
                                    let y0 = ((shot_rect.min.y * ppp).round().max(0.0) as usize).min(ih);
                                    let x1 = ((shot_rect.max.x * ppp).round().max(0.0) as usize).min(iw);
                                    let y1 = ((shot_rect.max.y * ppp).round().max(0.0) as usize).min(ih);
                                    if x1 <= x0 || y1 <= y0 {
                                        return;
                                    }
                                    let mut rgb = Vec::with_capacity((x1 - x0) * (y1 - y0) * 3);
                                    for y in y0..y1 {
                                        for x in x0..x1 {
                                            let c = img.pixels[y * iw + x];
                                            rgb.extend_from_slice(&[c.r(), c.g(), c.b()]);
                                        }
                                    }
                                    let _ = std::fs::write(path, crate::chart::png((x1 - x0) as u32, (y1 - y0) as u32, &rgb));
                                });
                            }
                        }
                    },
                );
                keep
            });
            self.charts = wins;
        }
    }
}

/// The first disc drive that holds a disc (or the first drive), so the program starts on it.
fn default_source() -> Option<String> {
    #[cfg(windows)]
    {
        return ('D'..='Z')
            .map(|l| format!("{l}:\\"))
            .find(|p| Path::new(&format!("{p}BDMV")).exists());
    }
    #[cfg(not(windows))]
    {
        let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let mut found: Vec<String> = Vec::new();
        for l in mounts.lines() {
            let mut f = l.split_whitespace();
            let (dev, dir, fs) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""));
            let optical =
                dev.starts_with("/dev/sr") || dev.starts_with("/dev/scd") || dev.starts_with("/dev/cdrom") || dev.starts_with("/dev/dvd");
            if optical && matches!(fs, "udf" | "iso9660") {
                found.push(dir.replace("\\040", " "));
            }
        }
        // with several drives, prefer the one that holds a Blu-ray
        if let Some(d) = found.iter().find(|d| Path::new(d).join("BDMV").exists()).or(found.first()) {
            return Some(d.clone());
        }
        ["/dev/sr0", "/dev/sr1", "/dev/cdrom", "/dev/dvd"]
            .iter()
            .find(|p| Path::new(p).exists())
            .map(|p| p.to_string())
    }
}

pub fn run(initial: Option<String>) {
    let sh: Sh = Arc::new(Mutex::new(Shared::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let watch = Arc::new(Mutex::new(String::new()));
    let gone = Arc::new(AtomicBool::new(false));
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TuxInfo")
            .with_app_id("tuxinfo")
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../packaging/icons/tuxinfo-256.png")).unwrap_or_default())
            .with_inner_size([900.0, 960.0])
            .with_min_inner_size([560.0, 460.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    let (sh2, cancel2) = (sh.clone(), cancel.clone());
    let r = eframe::run_native(
        "TuxInfo",
        opts,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            apply_theme(&ctx, Settings::load().dark);
            ctx.global_style_mut(|s| s.spacing.scroll = egui::style::ScrollStyle::solid());
            let (sh3, c3) = (sh2.clone(), cancel2.clone());
            let (w2, g2, ctx2) = (watch.clone(), gone.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let src = w2.lock().unwrap().clone();
                if !src.is_empty() && !medium_present(&src) {
                    g2.store(true, Ordering::Relaxed);
                    ctx2.request_repaint();
                }
            });
            std::thread::spawn(move || worker(rx, sh3, c3, ctx));
            let mut app = App {
                sh: sh2,
                tx,
                cancel: cancel2,
                src: String::new(),
                rows: Arc::new(Vec::new()),
                rows_ver: 0,
                report_ver: 0,
                checked: HashSet::new(),
                sel: None,
                set: Settings::load(),
                short_buf: String::new(),
                saved: Settings::load(),
                snap: Settings::load(),
                themed: Settings::load().dark,
                show_about: false,
                logo: None,
                upd: Arc::new(Mutex::new(String::new())),
                styled: 0.0,
                pending: Settings::load().font,
                wts: [96.0 / 404.0, 100.0 / 404.0, 150.0 / 404.0, 58.0 / 404.0],
                cols: [
                    vec![0.22, 0.09, 0.14, 0.28, 0.27],
                    vec![0.22, 0.09, 0.14, 0.28, 0.27],
                    vec![0.30, 0.17, 0.15, 0.38],
                ],
                opened: false,
                watch,
                gone,
                show_settings: false,
                show_custom: false,
                cd: CustomDlg::default(),
                custom_count: 0,
                report_names: Vec::new(),
                csel: [0; 4],
                want_chart: false,
                chart_note: None,
                chart_ver: 0,
                chart_seq: 0,
                charts: Vec::new(),
                show_report: false,
                report: String::new(),
                want_report: false,
                sel_clip: None,
                #[cfg(all(feature = "preview", unix))]
                preview: None,
                play_err: None,
                scan_msg: None,
            };
            match initial {
                Some(p) => app.open(p),
                None => app.src = default_source().unwrap_or_default(),
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = r {
        eprintln!("tuxinfo: cannot start the GUI: {e}");
        std::process::exit(1);
    }
}

/// False once the disc behind `src` is gone: a drive with no medium (the kernel reports size 0), a mounted
/// disc that was unmounted, or a path that no longer exists. Only looks at sysfs and /proc, so it never wakes the drive.
fn medium_present(src: &str) -> bool {
    let p = std::path::Path::new(src);
    let mut dev = src.starts_with("/dev/").then(|| src.to_string());
    if dev.is_none() {
        if let (Ok(c), Ok(mi)) = (std::fs::canonicalize(p), std::fs::read_to_string("/proc/self/mountinfo")) {
            // longest mount point that contains the path
            let mut best = 0;
            for l in mi.lines() {
                let f: Vec<&str> = l.split(' ').collect();
                let Some(sep) = f.iter().position(|x| *x == "-") else { continue };
                let (mp, source) = (f.get(4).copied().unwrap_or(""), f.get(sep + 2).copied().unwrap_or(""));
                let mp = mp.replace("\\040", " ");
                if source.starts_with("/dev/") && c.starts_with(&mp) && mp.len() >= best {
                    best = mp.len();
                    dev = Some(source.to_string());
                }
            }
        }
    }
    if let Some(dev) = dev.and_then(|d| std::fs::canonicalize(d).ok()) {
        let name = dev.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if std::fs::read_to_string(format!("/sys/class/block/{name}/size")).is_ok_and(|s| s.trim() == "0") {
            return false;
        }
        // An optical drive's sysfs size can stay stale after the tray opens, so ask the drive itself
        // (CDROM_DRIVE_STATUS on a non-blocking handle: no media access, no spin-up).
        if name.starts_with("sr") || name.starts_with("scd") {
            use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};
            extern "C" {
                fn ioctl(fd: i32, req: u64, ...) -> i32;
            }
            if let Ok(f) = std::fs::OpenOptions::new().read(true).custom_flags(0o4000).open(&dev) {
                // CDROM_DRIVE_STATUS = 0x5326, slot argument CDSL_CURRENT = i32::MAX
                // SAFETY: `f` is an open file descriptor for the whole call and this request takes one integer argument.
                let st = unsafe { ioctl(f.as_raw_fd(), 0x5326, i32::MAX) };
                if st == 1 || st == 2 {
                    // CDS_NO_DISC / CDS_TRAY_OPEN
                    return false;
                }
            }
        }
    }
    p.exists()
}

/// A real OS window (can leave the main window) whose contents are drawn by `f`; false once closed.
pub(crate) fn os_window(ctx: &egui::Context, id: &str, title: &str, size: [f32; 2], mut f: impl FnMut(&mut egui::Ui)) -> bool {
    let mut open = true;
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of(id),
        egui::ViewportBuilder::default()
            .with_title(title)
            .with_app_id("tuxinfo")
            .with_inner_size(size),
        |ctx, _| {
            egui::CentralPanel::default().show(ctx, |ui| f(ui));
            if ctx.input(|i| i.viewport().close_requested()) {
                open = false;
            }
        },
    );
    open
}
