//! Right-hand panel: event log, controls, audio mixer, chat, stats.
//! Extracted verbatim from `app.rs`.

use super::super::state::EventLogKind;
use super::super::RobsApp;
use eframe::egui;

impl RobsApp {
    pub(crate) fn right_panel(&mut self, ctx: &egui::Context) {
        if self.show_audio || self.show_chat || self.show_stats || self.show_controls || self.show_event_log {
            egui::SidePanel::right("right_panel")
                .default_width(300.0)
                .min_width(120.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if self.show_event_log {
                            let log_count = self.event_log.len();
                            ui.add_space(8.0);
                            egui::Frame::group(ui.style())
                                .fill(egui::Color32::from_rgb(28, 28, 32))
                                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 80)))
                                .rounding(egui::Rounding::same(4.0))
                                .inner_margin(egui::Margin::same(6.0))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.add_space(5.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("EVENT LOG ({})", log_count))
                                                .strong()
                                                .color(egui::Color32::from_rgb(200, 200, 210)),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.menu_button(
                                                    egui::RichText::new("...").color(egui::Color32::from_rgb(160, 160, 170)),
                                                    |ui| {
                                                        if ui.button("Clear Log").clicked() {
                                                            self.event_log.clear();
                                                            ui.close_menu();
                                                        }
                                                        if ui.button("Export PDF...").clicked() {
                                                            ui.close_menu();
                                                            self.export_event_log_pdf();
                                                        }
                                                    },
                                                );
                                            },
                                        );
                                    });
                                    ui.add_space(5.0);
                                    let header_bottom = ui.min_rect().max.y;
                                    ui.painter().hline(
                                        ui.min_rect().min.x..=ui.min_rect().max.x,
                                        header_bottom + 2.0,
                                        egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 80)),
                                    );
                                    ui.add_space(10.0);
                                    egui::ScrollArea::vertical()
                                        .max_height(220.0)
                                        .stick_to_bottom(true)
                                        .show(ui, |ui| {
                                            if self.event_log.is_empty() {
                                                ui.label(
                                                    egui::RichText::new("No events yet")
                                                        .color(egui::Color32::from_rgb(90, 90, 90))
                                                        .italics(),
                                                );
                                            } else {
                                                let entries: Vec<_> = self.event_log.iter().rev().collect();
                                                let total = entries.len();
                                                for (idx, entry) in entries.iter().enumerate() {
                                                    let time_str = entry.timestamp.format("%H:%M:%S").to_string();
                                                    let color = match entry.kind {
                                                        EventLogKind::Stream => egui::Color32::from_rgb(100, 180, 255),
                                                        EventLogKind::Record => egui::Color32::from_rgb(255, 130, 130),
                                                        EventLogKind::Annotation => egui::Color32::from_rgb(180, 220, 130),
                                                        EventLogKind::Overlay => egui::Color32::from_rgb(220, 190, 255),
                                                        EventLogKind::Info => egui::Color32::from_rgb(180, 180, 180),
                                                    };
                                                    ui.horizontal(|ui| {
                                                        ui.label(
                                                            egui::RichText::new(&time_str)
                                                                .color(egui::Color32::from_rgb(120, 160, 120))
                                                                .monospace(),
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(&entry.message).color(color),
                                                        );
                                                    });
                                                    if idx < total - 1 {
                                                        ui.separator();
                                                    }
                                                }
                                            }
                                        });
                                });
                            ui.add_space(4.0);
                        }

                        if self.show_controls {
                            ui.collapsing("Controls", |ui| {
                                ui.vertical_centered(|ui| {
                                    // ---- Streaming controls ----
let (s_icon, _s_label, s_color) = if !self.streaming {
                                        ("\u{25B6}", "Start", egui::Color32::from_rgb(0, 170, 0))
                                    } else if self.streaming_paused {
                                        ("\u{25B6}", "Resume", egui::Color32::from_rgb(0, 170, 0))
                                    } else {
                                        ("\u{23F8}", "Pause", egui::Color32::from_rgb(210, 160, 0))
                                    };
                                    if ui.add(egui::Button::new(
                                        egui::RichText::new(format!("{} Stream", s_icon)).color(s_color).strong(),
                                    )).clicked() {
                                        if !self.streaming {
                                            self.start_streaming();
                                        } else if self.streaming_paused {
                                            self.streaming_paused = false;
                                            self.log_event("Streaming resumed", EventLogKind::Stream);
                                        } else {
                                            self.streaming_paused = true;
                                            self.log_event("Streaming paused", EventLogKind::Stream);
                                        }
                                    }
                                    if ui.add_enabled(
                                        self.streaming,
                                        egui::Button::new(egui::RichText::new("\u{23F9} Stop Stream").color(egui::Color32::RED)),
                                    ).clicked() {
                                        self.stop_streaming();
                                    }

                                    ui.separator();

                                    // ---- Recording controls ----
let (r_icon, _r_label, r_color) = if !self.record.recording {
                                        ("\u{25B6}", "Start", egui::Color32::from_rgb(0, 170, 0))
                                    } else if self.record.recording_paused {
                                        ("\u{25B6}", "Resume", egui::Color32::from_rgb(0, 170, 0))
                                    } else {
                                        ("\u{23F8}", "Pause", egui::Color32::from_rgb(210, 160, 0))
                                    };
                                    if ui.add(egui::Button::new(
                                        egui::RichText::new(format!("{} Record", r_icon)).color(r_color).strong(),
                                    )).clicked() {
                                        if !self.record.recording {
                                            self.start_recording();
                                        } else if self.record.recording_paused {
                                            self.record.recording_paused = false;
                                            self.log_event("Recording resumed", EventLogKind::Record);
                                        } else {
                                            self.record.recording_paused = true;
                                            self.log_event("Recording paused", EventLogKind::Record);
                                        }
                                    }
                                    if ui.add_enabled(
                                        self.record.recording,
                                        egui::Button::new(egui::RichText::new("\u{23F9} Stop Record").color(egui::Color32::RED)),
                                    ).clicked() {
                                        self.stop_recording();
                                    }
                                    ui.separator();
                                    if ui.button("Studio Mode").clicked() {}
                                    if ui.button("Settings").clicked() {
                                        self.show_settings = true;
                                    }
                                });
                            });
                        }

                        if self.show_audio {
                            ui.collapsing("Audio Mixer", |ui| {
                                for ch in self.audio_channels.iter_mut() {
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut ch.muted, "M");
                                        ui.label(&ch.name);
                                        ui.add(
                                            egui::Slider::new(&mut ch.volume, 0.0..=1.0)
                                                .show_value(false),
                                        );
                                        ui.label(format!("{:.0}%", ch.volume * 100.0));
                                    });
                                    let vol = ch.volume;
                                    let meter_color = if vol > 0.9 {
                                        egui::Color32::RED
                                    } else if vol > 0.7 {
                                        egui::Color32::YELLOW
                                    } else {
                                        egui::Color32::GREEN
                                    };
                                    let meter_rect = ui.available_rect_before_wrap();
                                    let meter = egui::Rect::from_min_size(
                                        meter_rect.min,
                                        egui::vec2(meter_rect.width() * vol, 6.0),
                                    );
                                    ui.painter().rect_filled(
                                        meter,
                                        1.0,
                                        meter_color.gamma_multiply(0.7),
                                    );
                                    ui.add_space(10.0);
                                }
                            });
                        }

                        if self.show_chat {
                            ui.collapsing("Chat", |ui| {
                                egui::ScrollArea::vertical()
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        let messages = self.chat_messages.read();
                                        if messages.is_empty() {
                                            ui.label(
                                                egui::RichText::new("No messages")
                                                    .color(egui::Color32::GRAY),
                                            );
                                        } else {
                                            for msg in messages.iter() {
                                                ui.horizontal(|ui| {
                                                    let color = msg
                                                        .user
                                                        .color
                                                        .as_ref()
                                                        .and_then(|c| {
                                                            egui::Color32::from_hex(c).ok()
                                                        })
                                                        .unwrap_or(egui::Color32::WHITE);
                                                    ui.label(
                                                        egui::RichText::new(&msg.user.display_name)
                                                            .color(color)
                                                            .strong(),
                                                    );
                                                    ui.label(":");
                                                    ui.label(&msg.content);
                                                });
                                            }
                                        }
                                    });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.text_edit_singleline(&mut self.chat_input);
                                    if ui.button("Send").clicked() && !self.chat_input.is_empty() {
                                        self.chat_input.clear();
                                    }
                                });
                            });
                        }

                        if self.show_stats {
                            ui.collapsing("Stats", |ui| {
                                egui::Grid::new("stats").show(ui, |ui| {
                                    ui.label("Streaming:");
                                    ui.label(if self.streaming { "Active" } else { "Inactive" });
                                    ui.end_row();
                                    ui.label("Recording:");
                                    ui.label(if self.record.recording { "Active" } else { "Inactive" });
                                    ui.end_row();
                                    if self.streaming {
                                        ui.label("Duration:");
                                        ui.label(Self::format_time(self.streaming_time));
                                        ui.end_row();
                                    }
                                    ui.label("Frame Rate:");
                                    ui.label(format!("{:.1} fps", self.fps));
                                    ui.end_row();
                                    ui.label("Bitrate:");
                                    ui.label(format!("{} kbps", self.bitrate));
                                    ui.end_row();
                                    ui.label("Dropped Frames:");
                                    ui.label(format!("{}", self.dropped_frames));
                                    ui.end_row();
                                });
                            });
                        }

                    // --- Event Log section moved to top ---
                });
            });
        }
    }

    /// Export the current event log as a paginated PDF report.
    ///
    /// Shows a native save dialog (rfd, same as the settings folder pickers),
    /// builds a chronological `Report` — the in-app log shows newest-first, but
    /// a report reads oldest-first — and writes it with the dependency-free
    /// PDF writer in `robs_outputs::report`. The outcome is logged back into
    /// the event log itself.
    fn export_event_log_pdf(&mut self) {
        if self.event_log.is_empty() {
            self.log_event("Event log is empty - nothing to export", EventLogKind::Info);
            return;
        }

        let default_name = format!(
            "robs_event_log_{}.pdf",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        );
        let start_dir =
            std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users".to_string());
        let Some(path) = rfd::FileDialog::new()
            .set_directory(start_dir)
            .set_file_name(default_name)
            .add_filter("PDF report", &["pdf"])
            .save_file()
        else {
            return; // user cancelled the dialog
        };

        let mut report = robs_outputs::Report::new("ROBS Event Log Report", "Session event log export");
        report.push_meta(
            "Generated",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        );
        report.push_meta("Entries", self.event_log.len().to_string());
        report.push_meta(
            "Application",
            format!("ROBS {}", robs_core::ROBS_VERSION),
        );
        for entry in &self.event_log {
            let kind = match entry.kind {
                EventLogKind::Stream => "Stream",
                EventLogKind::Record => "Record",
                EventLogKind::Annotation => "Annotation",
                EventLogKind::Overlay => "Overlay",
                EventLogKind::Info => "Info",
            };
            report.push_line(format!(
                "{}  [{:<10}]  {}",
                entry.timestamp.format("%Y-%m-%d %H:%M:%S"),
                kind,
                entry.message
            ));
        }

        match robs_outputs::report::write_pdf(&report, &path) {
            Ok(()) => self.log_event(
                format!("Event log exported to {}", path.display()),
                EventLogKind::Info,
            ),
            Err(e) => self.log_event(format!("PDF export failed: {e}"), EventLogKind::Info),
        }
    }
}
