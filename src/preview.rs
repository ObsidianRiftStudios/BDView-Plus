//! The preview window: video area, transport controls, audio/subtitle choice.

use crate::mpv::{Player, Target};
use eframe::egui::{self, RichText};
use std::sync::{Arc, Mutex};

pub struct Preview {
    player: Arc<Mutex<Player>>,
    pub open: bool,
    title: String,
    /// Position being dragged on the seek bar (seconds), if any.
    seek: Option<f64>,
}

fn clock(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

impl Preview {
    pub fn new() -> Result<Preview, String> {
        Ok(Preview {
            player: Arc::new(Mutex::new(Player::new()?)),
            open: false,
            title: String::new(),
            seek: None,
        })
    }

    pub fn play(&mut self, title: String, t: Target) {
        self.player.lock().unwrap().play(t);
        self.title = title;
        self.open = true;
    }

    /// Stop playing and close the window (the disc went away).
    pub fn stop(&mut self) {
        self.player.lock().unwrap().stop();
        self.open = false;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        self.player.lock().unwrap().poll();
        let player = self.player.clone();
        let seek = &mut self.seek;
        let line = ctx.global_style().text_styles[&egui::TextStyle::Body].size + 12.0;
        let title = format!("Preview - {}", self.title);
        let open = crate::gui::os_window(ctx, "preview", &title, [1280.0, 760.0], |ui| {
            // The guard must be gone before the window is painted: the paint callback locks the player too.
            let mut p = player.lock().unwrap();
            let video = (ui.available_height() - 3.0 * line - 18.0).max(120.0);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), video), egui::Sense::click());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            let pl = player.clone();
            ui.painter().add(egui::PaintCallback {
                rect,
                callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, painter| {
                    pl.lock().unwrap().paint(&info, painter.gl());
                })),
            });
            if resp.clicked() {
                p.toggle_pause();
            }

            // seek bar
            let dur = p.st.dur.max(0.1);
            let mut pos = (*seek).unwrap_or(p.st.pos).clamp(0.0, dur);
            ui.horizontal(|ui| {
                ui.label(clock(pos));
                ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(60.0);
                let r = ui.add(egui::Slider::new(&mut pos, 0.0..=dur).show_value(false));
                if r.dragged() || r.changed() {
                    *seek = Some(pos);
                }
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    p.seek(pos);
                    *seek = None;
                }
                ui.label(clock(dur));
            });

            ui.horizontal(|ui| {
                if ui.button(if p.st.paused { "Play" } else { "Pause" }).clicked() {
                    p.toggle_pause();
                }
                if ui.button("Stop").clicked() {
                    p.stop();
                }
                let mut vol = p.st.volume;
                ui.label("Volume");
                if ui
                    .add(egui::Slider::new(&mut vol, 0.0..=130.0).show_value(false).step_by(1.0))
                    .changed()
                {
                    p.set_volume(vol);
                }
                for (kind, name) in [("audio", "Audio"), ("sub", "Subtitles")] {
                    let tr: Vec<_> = p.st.tracks.iter().filter(|t| t.kind == kind).cloned().collect();
                    if tr.is_empty() {
                        continue;
                    }
                    let cur = tr.iter().find(|t| t.selected).map_or("Off".to_string(), |t| t.label.clone());
                    let mut pick: Option<i64> = None;
                    egui::ComboBox::from_id_salt(kind)
                        .selected_text(RichText::new(format!("{name}: {cur}")))
                        .width(190.0)
                        .show_ui(ui, |ui| {
                            if kind == "sub" && ui.selectable_label(!tr.iter().any(|t| t.selected), "Off").clicked() {
                                pick = Some(0);
                            }
                            for t in &tr {
                                if ui.selectable_label(t.selected, &t.label).clicked() {
                                    pick = Some(t.id);
                                }
                            }
                        });
                    if let Some(id) = pick {
                        p.select(kind, id);
                    }
                }
            });
            // always present, so the window height does not jump when a message appears
            let hw = match p.st.hw.as_str() {
                "" => String::new(),
                "no" => "Video decoder: software (no hardware decoder in use)".to_string(),
                h => format!("Video decoder: {h} (hardware)"),
            };
            let m = if !p.st.msg.is_empty() {
                p.st.msg.clone()
            } else if !hw.is_empty() {
                hw
            } else {
                " ".to_string()
            };
            ui.add(egui::Label::new(RichText::new(m).weak()).truncate());
        });
        let mut p = self.player.lock().unwrap();
        let busy = p.st.playing && !p.st.paused;
        ctx.request_repaint_after(std::time::Duration::from_millis(if busy { 16 } else { 100 }));
        if !open {
            p.stop();
            self.open = false;
        }
    }
}
