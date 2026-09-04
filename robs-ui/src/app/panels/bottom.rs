//! Bottom streaming-controls strip (scene name + LIVE/REC status). Extracted
//! verbatim from `app.rs`, minus a stray debug label that did not belong here.

use robs_controller::RobsController;
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
                                egui::RichText::new("\u{23F8} PAUSED")
                                    .color(egui::Color32::from_rgb(210, 160, 0))
                            } else {
                                egui::RichText::new("\u{25CF} LIVE").color(egui::Color32::RED)
                            };
                            ui.label(live_text);
                            ui.label(RobsController::format_time(self.streaming_time / 1000));
                        }
                        if self.record.recording {
                            let rec_text = if self.record.recording_paused {
                                egui::RichText::new("\u{23F8} PAUSED")
                                    .color(egui::Color32::from_rgb(210, 160, 0))
                            } else {
                                egui::RichText::new("\u{25CF} REC").color(egui::Color32::RED)
                            };
                            ui.label(rec_text);
                            ui.label(RobsController::format_time(self.record.recording_time / 1000));
                            // Clip-mark badge: bright while a mark is open, dim
                            // with the queued count otherwise.
                            if self.record.clip_mark_start.is_some() {
                                ui.label(
                                    egui::RichText::new("\u{23F1} MARK")
                                        .color(egui::Color32::from_rgb(255, 200, 60))
                                        .strong(),
                                );
                            } else if !self.record.clip_marks.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "\u{23F1} {}",
                                        self.record.clip_marks.len()
                                    ))
                                    .color(egui::Color32::from_rgb(180, 140, 60)),
                                );
                            }
                        }
                        // Clip export progress (post-stop background cutting).
                        if self.record.clip_export_pending > 0 {
                            ui.label(
                                egui::RichText::new(format!(
                                    "\u{23F3} clips {}",
                                    self.record.clip_export_pending
                                ))
                                .color(egui::Color32::from_rgb(210, 160, 0)),
                            );
                        }
                        // Blackbox safety-recorder health chip (no-op when disabled/idle).
                        self.blackbox_status_chip(ui);
                        // Anomaly clip-buffer controls (Start/Stop + Capture Clip + chip).
                        self.anomaly_controls(ui);
                    });
                });
                ui.add_space(2.0);
            });
    }
}
