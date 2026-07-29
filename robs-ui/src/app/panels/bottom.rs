//! Bottom streaming-controls strip (scene name + LIVE/REC status). Extracted
//! verbatim from `app.rs`, minus a stray debug label that did not belong here.

use super::super::RobsApp;
use eframe::egui;

impl RobsApp {
    pub(crate) fn streaming_controls(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("streaming_controls")
        .resizable(true)
        .min_height(50.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            let panel_h = ui.available_height();
            ui.horizontal(|ui| {
                ui.set_min_height(panel_h - 6.0);
                ui.label(format!("Scene: {}", self.current_scene));
                ui.separator();

                // ---- Status indicators (right-aligned) ----
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.streaming {
                        let live_text = if self.streaming_paused {
                            egui::RichText::new("\u{23F8} PAUSED").color(egui::Color32::from_rgb(210, 160, 0))
                        } else {
                            egui::RichText::new("\u{25CF} LIVE").color(egui::Color32::RED)
                        };
                        ui.label(live_text);
                        ui.label(Self::format_time(self.streaming_time));
                    }
                    if self.record.recording {
                        let rec_text = if self.record.recording_paused {
                            egui::RichText::new("\u{23F8} PAUSED").color(egui::Color32::from_rgb(210, 160, 0))
                        } else {
                            egui::RichText::new("\u{25CF} REC").color(egui::Color32::RED)
                        };
                        ui.label(rec_text);
                        ui.label(Self::format_time(self.record.recording_time));
                    }
                });
            });
            ui.add_space(2.0);
        });
    }
}
