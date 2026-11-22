//! Bitrate / frame-size charts (port of BDInfo's FormChart): the numbers are computed from the
//! per-file stream diagnostics gathered during the scan, then drawn with egui's painter.

use crate::disc::{Disc, Playlist};
use crate::report::n0;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke};
use std::collections::VecDeque;

pub const TYPES: [&str; 6] = [
    "Video Bitrate: 1-Second Window",
    "Video Bitrate: 5-Second Window",
    "Video Bitrate: 10-Second Window",
    "Video Frame Size (Min / Max)",
    "Video Frame Type Counts",
    "Video Frame Type Sizes",
];

pub struct Slice {
    pub label: String,
    pub count: f64,
    pub gray: u8,
}

pub enum Kind {
    /// (minutes, value) points of the three curves.
    Lines { min: Vec<[f64; 2]>, avg: Vec<[f64; 2]>, max: Vec<[f64; 2]> },
    Bars { labels: Vec<String>, avg: Vec<f64>, peak: Vec<f64> },
    Pie(Vec<Slice>),
}

pub struct Chart {
    pub title: String,
    pub window_title: String,
    pub file: String,
    pub unit: &'static str,
    pub xt: String,
    pub yt: String,
    pub total_min: f64,
    pub kind: Kind,
}

/// Numbers for one chart. `kind` indexes [`TYPES`].
pub fn build(d: &Disc, pl: &Playlist, kind: usize, pid: u16, angle: usize) -> Chart {
    let ty = TYPES[kind.min(TYPES.len() - 1)];
    let label = d.label.replace(' ', "_");
    let stem = pl.name.rsplit_once('.').map_or(pl.name.as_str(), |x| x.0);
    let file = format!("{label}-{stem}-{}.png", ["bitrate-01s", "bitrate-05s", "bitrate-10s", "frame-size", "frame-type-count", "frame-type-size"][kind.min(5)]);
    let total = pl.total_length();
    let mut c = Chart {
        title: ty.to_string(),
        window_title: format!("{}: {}", pl.name, ty),
        file,
        unit: "",
        xt: "Time (minutes)".into(),
        yt: String::new(),
        total_min: total / 60.0,
        kind: Kind::Pie(Vec::new()),
    };
    match kind {
        0..=2 => {
            c.unit = "Mbps";
            c.yt = "Bitrate (Mbps)".into();
            c.kind = window_chart(d, pl, pid, angle, [1.0, 5.0, 10.0][kind], total);
        }
        3 => {
            c.unit = "KB";
            c.yt = "Size (KB)".into();
            c.kind = size_chart(d, pl, pid, angle, total);
        }
        _ => frame_types(d, pl, pid, angle, kind == 5, &mut c),
    }
    c
}

/// Did the scan record frames for this stream?
pub fn has_data(d: &Disc, pl: &Playlist, pid: u16, angle: usize) -> bool {
    clips(d, pl, pid, angle).any(|(_, g)| !g.is_empty())
}

/// The diagnostics of every clip of the chosen angle, with the clip they belong to.
fn clips<'a>(d: &'a Disc, pl: &'a Playlist, pid: u16, angle: usize) -> impl Iterator<Item = (&'a crate::disc::Clip, &'a Vec<crate::disc::Diag>)> {
    pl.clips.iter().filter(move |c| c.angle == angle).filter_map(move |c| Some((c, d.files.get(&c.name)?.diag.get(&pid)?)))
}

/// Shared "one point per second" bookkeeping of the original (min / avg / max curves).
struct Points {
    min: Vec<[f64; 2]>,
    avg: Vec<[f64; 2]>,
    max: Vec<[f64; 2]>,
    sec: f64,
    pmin: f64,
    pmax: f64,
    pavg: f64,
    count: u32,
}

impl Points {
    fn new() -> Self {
        Points { min: vec![], avg: vec![], max: vec![], sec: 1.0, pmin: f64::MAX, pmax: 0.0, pavg: 0.0, count: 0 }
    }
    fn zero(&mut self, x: f64) {
        let px = (x - 1.0) / 60.0;
        self.min.push([px, 0.0]);
        self.avg.push([px, 0.0]);
        self.max.push([px, 0.0]);
    }
    /// Fill silent seconds, then emit the current second.
    fn emit(&mut self, pos: f64) {
        let mut x = self.sec;
        while x < pos - 1.0 {
            self.zero(x);
            self.sec += 1.0;
            x += 1.0;
        }
        let m = (self.sec - 1.0) / 60.0;
        if self.count > 0 {
            self.min.push([m, self.pmin]);
            self.avg.push([m, self.pavg / self.count as f64]);
            self.max.push([m, self.pmax]);
        } else {
            self.zero(self.sec);
        }
        self.pmin = f64::MAX;
        self.pmax = 0.0;
        self.pavg = 0.0;
        self.count = 0;
        self.sec += 1.0;
    }
    fn finish(mut self, total: f64) -> Kind {
        let mut x = self.sec;
        while x < total {
            self.zero(x);
            x += 1.0;
        }
        Kind::Lines { min: self.min, avg: self.avg, max: self.max }
    }
}

fn window_chart(d: &Disc, pl: &Playlist, pid: u16, angle: usize, window: f64, total: f64) -> Kind {
    let mut p = Points::new();
    let (mut bits, mut secs): (VecDeque<f64>, VecDeque<f64>) = (VecDeque::new(), VecDeque::new());
    let (mut bsum, mut ssum) = (0.0f64, 0.0f64);
    for (c, diag) in clips(d, pl, pid, angle) {
        for g in diag {
            let pos = g.marker - c.time_in + c.rel_in;
            let (s, b) = (g.interval, g.bytes as f64 * 8.0);
            ssum += s;
            secs.push_back(s);
            bsum += b;
            bits.push_back(b);
            if ssum > window {
                let r = bsum / ssum / 1_000_000.0;
                p.pmin = p.pmin.min(r);
                p.pmax = p.pmax.max(r);
                p.count += 1;
                p.pavg += r;
                if pos >= p.sec {
                    p.emit(pos);
                }
                bsum -= bits.pop_front().unwrap_or(0.0);
                ssum -= secs.pop_front().unwrap_or(0.0);
            }
            if pos >= p.sec {
                p.emit(pos);
            }
        }
    }
    p.finish(total)
}

fn size_chart(d: &Disc, pl: &Playlist, pid: u16, angle: usize, total: f64) -> Kind {
    let mut p = Points::new();
    for (c, diag) in clips(d, pl, pid, angle) {
        for g in diag.iter().filter(|g| !g.tag.is_empty()) {
            let size = (g.bytes / 1024) as f64;
            let pos = g.marker - c.time_in + c.rel_in;
            p.pmin = p.pmin.min(size);
            p.pmax = p.pmax.max(size);
            p.count += 1;
            p.pavg += size;
            if pos >= p.sec {
                p.emit(pos);
            }
        }
    }
    p.finish(total)
}

fn frame_types(d: &Disc, pl: &Playlist, pid: u16, angle: usize, sizes: bool, c: &mut Chart) {
    // (type, count, total KB, peak KB) in order of first appearance
    let mut t: Vec<(String, f64, f64, f64)> = Vec::new();
    for (_, diag) in clips(d, pl, pid, angle) {
        for g in diag.iter().filter(|g| !g.tag.is_empty()) {
            let size = (g.bytes / 1024) as f64;
            let i = match t.iter().position(|x| x.0 == g.tag) {
                Some(i) => i,
                None => {
                    t.push((g.tag.to_string(), 0.0, 0.0, 0.0));
                    t.len() - 1
                }
            };
            t[i].1 += 1.0;
            t[i].2 += size;
            t[i].3 = t[i].3.max(size);
        }
    }
    c.xt = "Frame Type".into();
    if sizes {
        c.unit = "KB";
        c.yt = "Average / Peak Size (KB)".into();
        c.kind = Kind::Bars { labels: t.iter().map(|x| x.0.clone()).collect(), avg: t.iter().map(|x| x.2 / x.1).collect(), peak: t.iter().map(|x| x.3).collect() };
        return;
    }
    c.yt = "Count".into();
    let total: f64 = t.iter().map(|x| x.1).sum();
    let drgb = (255 / t.len().max(1)) as u32;
    let mut sorted: Vec<(String, f64)> = t.iter().map(|x| (x.0.clone(), x.1)).collect();
    sorted.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut out: Vec<Slice> = Vec::new();
    let mut rgb = 0u32;
    let mut add = |n: &(String, f64), rgb: &mut u32| {
        out.push(Slice {
            label: format!("{} Frames\n{}\n({:.2}%)", n.0, n0(n.1 as i64), n.1 / total * 100.0),
            count: n.1,
            gray: (*rgb).min(255) as u8,
        });
        *rgb += drgb;
    };
    let mut j = sorted.len();
    let mut i = 0;
    while i < j {
        add(&sorted[i], &mut rgb);
        j -= 1;
        if j > i {
            add(&sorted[j], &mut rgb);
        }
        i += 1;
    }
    c.kind = Kind::Pie(out);
}

/// Current zoom of a chart window: the visible range of the time axis, in minutes.
#[derive(Clone, Copy)]
pub struct View {
    pub x0: f64,
    pub x1: f64,
}

impl View {
    pub fn new(c: &Chart) -> View {
        View { x0: 0.0, x1: c.total_min.max(1e-6) }
    }
}

pub struct Drawn {
    /// Where the chart (title, axes and all) sits, in points; used to crop a saved image.
    pub rect: Rect,
    pub status: String,
}

fn nice_step(range: f64, target: f64) -> f64 {
    let raw = (range / target).max(1e-12);
    let mag = 10f64.powf(raw.log10().floor());
    let f = raw / mag;
    mag * if f <= 1.0 { 1.0 } else if f <= 2.0 { 2.0 } else if f <= 5.0 { 5.0 } else { 10.0 }
}

fn fmt_tick(v: f64, step: f64) -> String {
    if step >= 1.0 { format!("{v:.0}") } else if step >= 0.1 { format!("{v:.1}") } else if step >= 0.01 { format!("{v:.2}") } else { format!("{v:.3}") }
}

const GRID: Color32 = Color32::from_gray(222);
const INK: Color32 = Color32::from_gray(20);

/// Draw the chart into the space the Ui has left, handling zoom (wheel), pan (drag) and reset (double-click).
pub fn draw(ui: &mut egui::Ui, ch: &Chart, view: &mut View) -> Drawn {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, Color32::WHITE);
    let f = FontId::proportional(13.0);
    let title_f = FontId::proportional(17.0);
    p.text(Pos2::new(rect.center().x, rect.top() + 8.0), Align2::CENTER_TOP, &ch.title, title_f, INK);
    let pie = matches!(ch.kind, Kind::Pie(_));
    let cr = if pie { rect.shrink2(egui::vec2(20.0, 44.0)) } else { Rect::from_min_max(Pos2::new(rect.left() + 66.0, rect.top() + 40.0), Pos2::new(rect.right() - 66.0, rect.bottom() - 46.0)) };
    let hover = resp.hover_pos().filter(|h| cr.contains(*h));
    let mut status = String::new();
    if cr.width() < 50.0 || cr.height() < 50.0 {
        return Drawn { rect, status };
    }

    match &ch.kind {
        Kind::Lines { min, avg, max } => {
            // zoom / pan
            let span = view.x1 - view.x0;
            let full = ch.total_min.max(1e-6);
            if let Some(h) = hover {
                let sc = ui.input(|i| i.smooth_scroll_delta.y as f64 + (i.zoom_delta() as f64 - 1.0) * 400.0);
                if sc.abs() > 0.0 {
                    let fx = ((h.x - cr.left()) / cr.width()) as f64;
                    let anchor = view.x0 + fx * span;
                    let ns = (span * (-sc * 0.002).exp()).clamp(full / 2000.0, full);
                    view.x0 = anchor - fx * ns;
                    view.x1 = view.x0 + ns;
                }
            }
            if resp.dragged() {
                let dx = resp.drag_delta().x as f64 / cr.width() as f64 * (view.x1 - view.x0);
                view.x0 -= dx;
                view.x1 -= dx;
            }
            if resp.double_clicked() {
                *view = View::new(ch);
            }
            let span = view.x1 - view.x0;
            if view.x0 < 0.0 {
                view.x1 -= view.x0;
                view.x0 = 0.0;
            }
            if view.x1 > full {
                view.x0 = (view.x0 - (view.x1 - full)).max(0.0);
                view.x1 = full.max(view.x0 + span.min(full));
            }
            let (x0, x1) = (view.x0, view.x1);
            let ymax_data = max.iter().chain(avg.iter()).map(|q| q[1]).fold(0.0, f64::max);
            let ystep = nice_step(ymax_data.max(1e-6), 8.0);
            let ymax = ((ymax_data / ystep).floor() + 1.0) * ystep;
            let tx = |x: f64| cr.left() + ((x - x0) / (x1 - x0)) as f32 * cr.width();
            let ty = |y: f64| cr.bottom() - (y / ymax) as f32 * cr.height();
            // grid + ticks
            let xstep = nice_step(x1 - x0, (cr.width() / 90.0) as f64);
            let mut x = (x0 / xstep).ceil() * xstep;
            while x <= x1 + 1e-9 {
                let px = tx(x);
                p.vline(px, cr.top()..=cr.bottom(), Stroke::new(1.0, GRID));
                p.text(Pos2::new(px, cr.bottom() + 4.0), Align2::CENTER_TOP, fmt_tick(x, xstep), f.clone(), INK);
                x += xstep;
            }
            let mut y = 0.0;
            while y <= ymax + 1e-9 {
                let py = ty(y);
                p.hline(cr.left()..=cr.right(), py, Stroke::new(1.0, GRID));
                let t = fmt_tick(y, ystep);
                p.text(Pos2::new(cr.left() - 6.0, py), Align2::RIGHT_CENTER, &t, f.clone(), INK);
                p.text(Pos2::new(cr.right() + 6.0, py), Align2::LEFT_CENTER, t, f.clone(), INK);
                y += ystep;
            }
            // curves, clipped to the plot area; thinned out when zoomed far out
            let cp = p.with_clip_rect(cr);
            let vis = |s: &[[f64; 2]]| -> (usize, usize) {
                let a = s.partition_point(|q| q[0] < x0).saturating_sub(1);
                let b = (s.partition_point(|q| q[0] <= x1) + 1).min(s.len());
                (a, b)
            };
            let cap = (cr.width() as usize * 2).max(200);
            let thin = |s: &[[f64; 2]], mode: u8| -> Vec<Pos2> {
                let (a, b) = vis(s);
                let s = &s[a..b];
                let stride = (s.len() / cap).max(1);
                s.chunks(stride)
                    .map(|ch| {
                        let v = match mode {
                            0 => ch.iter().map(|q| q[1]).fold(f64::MAX, f64::min),
                            2 => ch.iter().map(|q| q[1]).fold(0.0, f64::max),
                            _ => ch.iter().map(|q| q[1]).sum::<f64>() / ch.len() as f64,
                        };
                        Pos2::new(tx(ch[0][0]), ty(v))
                    })
                    .collect()
            };
            let (pmin, pavg, pmax) = (thin(min, 0), thin(avg, 1), thin(max, 2));
            let fill = |pts: &[Pos2], col: Color32| {
                let mut m = egui::Mesh::default();
                for q in pts {
                    m.colored_vertex(*q, col);
                    m.colored_vertex(Pos2::new(q.x, cr.bottom()), col);
                }
                for i in 0..pts.len().saturating_sub(1) as u32 {
                    m.add_triangle(2 * i, 2 * i + 1, 2 * i + 2);
                    m.add_triangle(2 * i + 1, 2 * i + 3, 2 * i + 2);
                }
                cp.add(egui::Shape::mesh(m));
            };
            let light = Color32::from_gray(205);
            fill(&pmax, light);
            fill(&pmin, Color32::WHITE);
            cp.add(egui::Shape::line(pmax.clone(), Stroke::new(1.0, light)));
            cp.add(egui::Shape::line(pmin.clone(), Stroke::new(1.0, light)));
            cp.add(egui::Shape::line(pavg, Stroke::new(1.5, Color32::from_gray(110))));
            // legend
            let mut lx = cr.right() - 150.0;
            for (n, c) in [("Avg", Color32::from_gray(110)), ("Min", light), ("Max", light)] {
                cp.hline(lx..=lx + 16.0, cr.top() + 12.0, Stroke::new(2.0, c));
                cp.text(Pos2::new(lx + 20.0, cr.top() + 12.0), Align2::LEFT_CENTER, n, f.clone(), INK);
                lx += 50.0;
            }
            if let Some(h) = hover {
                let xm = x0 + ((h.x - cr.left()) / cr.width()) as f64 * (x1 - x0);
                let yv = (cr.bottom() - h.y) as f64 / cr.height() as f64 * ymax;
                p.vline(h.x, cr.top()..=cr.bottom(), Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 70)));
                let ms = (xm * 60_000.0).round().max(0.0) as u64;
                status = format!("Time: {:.3} sec ({:02}:{:02}:{:02}.{:02})   Value: {:.2} {}", xm * 60.0, ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000, yv, ch.unit);
            }
        }
        Kind::Bars { labels, avg, peak } => {
            let top = peak.iter().chain(avg.iter()).cloned().fold(0.0, f64::max);
            let ystep = nice_step(top.max(1e-6), 8.0);
            let ymax = ((top / ystep).floor() + 2.0) * ystep;
            let ty = |y: f64| cr.bottom() - (y / ymax) as f32 * cr.height();
            let mut y = 0.0;
            while y <= ymax + 1e-9 {
                p.hline(cr.left()..=cr.right(), ty(y), Stroke::new(1.0, GRID));
                p.text(Pos2::new(cr.left() - 6.0, ty(y)), Align2::RIGHT_CENTER, fmt_tick(y, ystep), f.clone(), INK);
                y += ystep;
            }
            let n = labels.len().max(1);
            let cw = cr.width() / n as f32;
            let bw = (cw * 0.36).min(70.0);
            for (i, l) in labels.iter().enumerate() {
                let cx = cr.left() + cw * (i as f32 + 0.5);
                let a = Rect::from_min_max(Pos2::new(cx - bw, ty(avg[i])), Pos2::new(cx, cr.bottom()));
                let k = Rect::from_min_max(Pos2::new(cx, ty(peak[i])), Pos2::new(cx + bw, cr.bottom()));
                p.rect_filled(a, 0.0, Color32::BLACK);
                p.rect_stroke(k, 0.0, Stroke::new(1.5, Color32::BLACK), egui::StrokeKind::Inside);
                p.text(Pos2::new(a.center().x, a.top() - 2.0), Align2::CENTER_BOTTOM, format!("{:.0}", avg[i]), f.clone(), INK);
                p.text(Pos2::new(k.center().x, k.top() - 2.0), Align2::CENTER_BOTTOM, format!("{:.0}", peak[i]), f.clone(), INK);
                p.text(Pos2::new(cx, cr.bottom() + 4.0), Align2::CENTER_TOP, l, f.clone(), INK);
            }
            let (lx, ly) = (cr.right() - 110.0, cr.top() + 10.0);
            p.rect_filled(Rect::from_min_size(Pos2::new(lx, ly), egui::vec2(14.0, 12.0)), 0.0, Color32::BLACK);
            p.text(Pos2::new(lx + 20.0, ly + 6.0), Align2::LEFT_CENTER, "Average", f.clone(), INK);
            p.rect_stroke(Rect::from_min_size(Pos2::new(lx, ly + 18.0), egui::vec2(14.0, 12.0)), 0.0, Stroke::new(1.5, Color32::BLACK), egui::StrokeKind::Inside);
            p.text(Pos2::new(lx + 20.0, ly + 24.0), Align2::LEFT_CENTER, "Peak", f.clone(), INK);
        }
        Kind::Pie(slices) => {
            let total: f64 = slices.iter().map(|s| s.count).sum::<f64>().max(1.0);
            let r = (cr.width().min(cr.height()) * 0.32).max(20.0);
            let c = cr.center();
            let mut a0 = -std::f32::consts::FRAC_PI_2;
            for s in slices {
                let sweep = (s.count / total) as f32 * std::f32::consts::TAU;
                let col = Color32::from_gray(s.gray);
                let n = ((sweep / 0.04).ceil() as usize).max(1);
                let mut m = egui::Mesh::default();
                m.colored_vertex(c, col);
                for k in 0..=n {
                    let a = a0 + sweep * k as f32 / n as f32;
                    m.colored_vertex(c + egui::vec2(a.cos(), a.sin()) * r, col);
                }
                for k in 0..n as u32 {
                    m.add_triangle(0, k + 1, k + 2);
                }
                p.add(egui::Shape::mesh(m));
                let mid = a0 + sweep / 2.0;
                let dir = egui::vec2(mid.cos(), mid.sin());
                let al = match (dir.x >= 0.0, dir.y) {
                    (true, y) if y < -0.5 => Align2::LEFT_BOTTOM,
                    (true, y) if y > 0.5 => Align2::LEFT_TOP,
                    (true, _) => Align2::LEFT_CENTER,
                    (false, y) if y < -0.5 => Align2::RIGHT_BOTTOM,
                    (false, y) if y > 0.5 => Align2::RIGHT_TOP,
                    (false, _) => Align2::RIGHT_CENTER,
                };
                p.text(c + dir * (r + 10.0), al, &s.label, f.clone(), INK);
                a0 += sweep;
            }
        }
    }
    if !pie {
        p.rect_stroke(cr, 0.0, Stroke::new(1.0, Color32::from_gray(60)), egui::StrokeKind::Inside);
        p.text(Pos2::new(cr.center().x, rect.bottom() - 6.0), Align2::CENTER_BOTTOM, &ch.xt, f.clone(), INK);
        let g = p.layout_no_wrap(ch.yt.clone(), f.clone(), INK);
        let w = g.size().x;
        p.add(egui::epaint::TextShape::new(Pos2::new(rect.left() + 6.0, cr.center().y + w / 2.0), g, INK).with_angle(-std::f32::consts::FRAC_PI_2));
        if matches!(ch.kind, Kind::Lines { .. }) {
            let g = p.layout_no_wrap(ch.yt.clone(), f.clone(), INK);
            let w = g.size().x;
            p.add(egui::epaint::TextShape::new(Pos2::new(rect.right() - 20.0, cr.center().y + w / 2.0), g, INK).with_angle(-std::f32::consts::FRAC_PI_2));
        }
    }
    Drawn { rect, status }
}

/// An uncompressed PNG (stored deflate blocks), enough to save a chart without extra libraries.
pub fn png(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = ty.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * w as usize * 3..(y + 1) * w as usize * 3]);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
