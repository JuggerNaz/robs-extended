//! Settings window and its per-tab renderers. Extracted verbatim from `app.rs`.

use super::RobsApp;
use eframe::egui;

impl RobsApp {
    pub(crate) fn show_settings_window(&mut self, ctx: &egui::Context) {
        let mut show_settings = self.show_settings;
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .resizable(true)
            .open(&mut show_settings)
            .default_width(650.0)
            .default_height(450.0)
            .min_width(400.0)
            .min_height(300.0)
            .show(ctx, |ui| {
                let available_height = ui.available_height();
                ui.columns(2, |cols| {
                    let left = &mut cols[0];
                    left.set_min_width(120.0);
                    left.set_max_width(160.0);
                    for (i, tab) in [
                        "General",
                        "Video",
                        "Audio",
                        "Hotkeys",
                        "Streaming",
                        "Outputs",
                        "Blackbox",
                    ]
                    .iter()
                    .enumerate()
                    {
                        if left
                            .selectable_label(self.active_settings_tab == i, *tab)
                            .clicked()
                        {
                            self.active_settings_tab = i;
                        }
                    }

                    let right = &mut cols[1];
                    egui::ScrollArea::vertical()
                        .max_height(available_height)
                        .show(right, |ui| {
                            ui.set_min_width(250.0);
                            match self.active_settings_tab {
                                0 => self.settings_general(ui),
                                1 => self.settings_video(ui),
                                2 => self.settings_audio(ui),
                                3 => self.settings_hotkeys(ui),
                                4 => self.settings_streaming(ui),
                                5 => self.settings_outputs(ui),
                                6 => self.settings_blackbox(ui),
                                _ => {}
                            }
                        });
                });
            });
        self.show_settings = show_settings;
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        ui.heading("General");
        ui.separator();
        egui::Grid::new("settings_general").show(ui, |ui| {
            ui.label("Language:");
            let _ = ui.button("English");
            ui.end_row();
            ui.label("Theme:");
            let _ = ui.button("Dark");
            ui.end_row();
            ui.label("Confirm on exit:");
            ui.checkbox(&mut self.confirm_on_exit, "");
            ui.end_row();
            ui.label("Minimize to tray:");
            ui.checkbox(&mut self.minimize_to_tray, "");
            ui.end_row();
            ui.label("Always on top:");
            ui.checkbox(&mut self.always_on_top, "");
            ui.end_row();
            ui.label("Check for updates:");
            ui.checkbox(&mut self.check_for_updates, "");
            ui.end_row();
            ui.label("Filename formatting:");
            ui.text_edit_singleline(&mut self.filename_formatting);
            ui.end_row();
        });
    }

    fn settings_video(&mut self, ui: &mut egui::Ui) {
        ui.heading("Video");
        ui.separator();
        egui::Grid::new("settings_video").show(ui, |ui| {
            ui.label("Base Resolution (Canvas):");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.base_width)
                        .range(640..=7680)
                        .suffix(" x"),
                );
                ui.add(egui::DragValue::new(&mut self.base_height).range(480..=4320));
            });
            ui.end_row();
            ui.label("Output (Scaled) Resolution:");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.output_width)
                        .range(640..=7680)
                        .suffix(" x"),
                );
                ui.add(egui::DragValue::new(&mut self.output_height).range(480..=4320));
            });
            ui.end_row();
            ui.label("Downscale Filter:");
            let _ = ui.button("Lanczos");
            ui.end_row();
            ui.label("FPS Type:");
            let _ = ui.button("Integer");
            ui.end_row();
            ui.label("FPS:");
            ui.add(egui::Slider::new(&mut self.fps_setting, 1.0..=120.0));
            ui.end_row();
        });
        ui.separator();
        ui.label(egui::RichText::new("Note: Base Resolution is your capture canvas size. Output Resolution is what gets encoded.").small().weak());
    }

    fn settings_audio(&mut self, ui: &mut egui::Ui) {
        ui.heading("Audio");
        ui.separator();
        egui::Grid::new("settings_audio").show(ui, |ui| {
            // Sample rate selection
            ui.label("Sample Rate:");
            egui::ComboBox::from_id_salt("audio_sample_rate")
                .selected_text(&self.audio_sample_rate)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.audio_sample_rate,
                        "44100".to_string(),
                        "44.1 kHz",
                    );
                    ui.selectable_value(&mut self.audio_sample_rate, "48000".to_string(), "48 kHz");
                });
            ui.end_row();

            // Channel mode selection
            ui.label("Channels:");
            egui::ComboBox::from_id_salt("audio_channel_mode")
                .selected_text(if self.audio_channel_mode == "mono" {
                    "Mono"
                } else {
                    "Stereo"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.audio_channel_mode, "mono".to_string(), "Mono");
                    ui.selectable_value(
                        &mut self.audio_channel_mode,
                        "stereo".to_string(),
                        "Stereo",
                    );
                });
            ui.end_row();

            // Desktop Audio device selection
            ui.label("Desktop Audio:");
            let desktop_device = self
                .audio_channels
                .iter()
                .find(|ch| ch.is_desktop)
                .map(|ch| ch.device_id.clone())
                .unwrap_or_else(|| "default".to_string());

            let mut selected_desktop = desktop_device.clone();
            egui::ComboBox::from_id_salt("desktop_audio_device")
                .selected_text(&selected_desktop)
                .show_ui(ui, |ui| {
                    for device in &self.audio_devices {
                        let display_name = if device.id == "disabled" {
                            "Disabled"
                        } else if device.id == "default" {
                            "Default"
                        } else {
                            &device.name
                        };
                        ui.selectable_value(&mut selected_desktop, device.id.clone(), display_name);
                    }
                });
            // Update the device_id in audio_channels
            for ch in &mut self.audio_channels {
                if ch.is_desktop {
                    ch.device_id = selected_desktop.clone();
                }
            }
            ui.end_row();

            // Mic/Aux device selection
            ui.label("Mic/Aux:");
            let mic_device = self
                .audio_channels
                .iter()
                .find(|ch| !ch.is_desktop)
                .map(|ch| ch.device_id.clone())
                .unwrap_or_else(|| "default".to_string());

            let mut selected_mic = mic_device.clone();
            egui::ComboBox::from_id_salt("mic_aux_device")
                .selected_text(&selected_mic)
                .show_ui(ui, |ui| {
                    for device in &self.audio_devices {
                        let display_name = if device.id == "disabled" {
                            "Disabled"
                        } else if device.id == "default" {
                            "Default"
                        } else {
                            &device.name
                        };
                        ui.selectable_value(&mut selected_mic, device.id.clone(), display_name);
                    }
                });
            // Update the device_id in audio_channels
            for ch in &mut self.audio_channels {
                if !ch.is_desktop {
                    ch.device_id = selected_mic.clone();
                }
            }
            ui.end_row();
        });
    }

    fn settings_hotkeys(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hotkeys");
        ui.separator();
        ui.label("Hotkey configuration coming soon.");
    }

    fn settings_streaming(&mut self, ui: &mut egui::Ui) {
        ui.heading("Streaming");
        ui.separator();
        egui::Grid::new("settings_streaming").show(ui, |ui| {
            ui.label("Service:");
            let _ = ui.button("Twitch");
            ui.end_row();
            ui.label("Server:");
            ui.text_edit_singleline(&mut self.stream_server);
            ui.end_row();
            ui.label("Stream Key:");
            ui.text_edit_singleline(&mut self.stream_key);
            ui.end_row();
            ui.label("Video Encoder:");
            egui::ComboBox::from_id_salt("video_encoder")
                .selected_text(&self.video_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_video_encoders {
                        ui.selectable_value(&mut self.video_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Audio Encoder:");
            egui::ComboBox::from_id_salt("audio_encoder")
                .selected_text(&self.audio_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_audio_encoders {
                        ui.selectable_value(&mut self.audio_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Bitrate:");
            ui.add(egui::Slider::new(&mut self.stream_bitrate, 1000..=20000).suffix(" kbps"));
            ui.end_row();
            ui.label("Rate Control:");
            let _ = ui.button("CBR");
            ui.end_row();
            ui.label("Keyframe Interval:");
            ui.add(egui::Slider::new(&mut self.keyframe_interval, 0..=20).suffix(" s"));
            ui.end_row();
            ui.label("Preset:");
            let _ = ui.button("faster");
            ui.end_row();
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Encoder Status:").strong());
            if self.ffmpeg_available {
                ui.label(egui::RichText::new("FFmpeg OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("FFmpeg not found").color(egui::Color32::RED));
            }
            if self.nvenc_available {
                ui.label(egui::RichText::new("NVENC OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("NVENC not available").color(egui::Color32::YELLOW));
            }
            if self.aac_available {
                ui.label(egui::RichText::new("AAC OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("AAC not available").color(egui::Color32::YELLOW));
            }
            });
    }

    fn settings_outputs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Outputs");
        ui.separator();
        ui.heading("Recording");
        ui.separator();
        egui::Grid::new("settings_recording").show(ui, |ui| {
            ui.label("Type:");
            let _ = ui.button("Standard");
            ui.end_row();
            ui.label("Format:");
            egui::ComboBox::from_id_salt("recording_format")
                .selected_text(&self.recording_format)
                .show_ui(ui, |ui| {
                    for fmt in ["mp4", "mkv", "flv", "mov"] {
                        ui.selectable_value(&mut self.recording_format, fmt.to_string(), fmt);
                    }
                });
            ui.end_row();
            ui.label("Video Encoder:");
            egui::ComboBox::from_id_salt("recording_encoder")
                .selected_text(&self.video_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_video_encoders {
                        ui.selectable_value(&mut self.video_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Audio Encoder:");
            egui::ComboBox::from_id_salt("audio_encoder")
                .selected_text(&self.audio_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_audio_encoders {
                        ui.selectable_value(&mut self.audio_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Bitrate:");
            ui.add(egui::Slider::new(&mut self.recording_bitrate, 1000..=50000).suffix(" kbps"));
            ui.end_row();
            ui.label("Path:");
            ui.horizontal(|ui| {
                let display_path = if self.recording_path.is_empty() {
                    "(not set)".to_string()
                } else {
                    self.recording_path.clone()
                };
                ui.label(display_path);
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_directory(
                            std::env::var("USERPROFILE")
                                .unwrap_or_else(|_| "C:\\Users".to_string()),
                        )
                        .pick_folder()
                    {
                    self.recording_path = path.to_string_lossy().into_owned();
                }
            }
            });
            ui.end_row();
        });
    }

    fn settings_blackbox(&mut self, ui: &mut egui::Ui) {
        ui.heading("Blackbox Safety Recorder");
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Always-on background recorder. Runs independently of the Record \
                 button whenever a capture source is active, writing rotating \
                 Matroska segments for crash-safe recovery.",
            )
            .small()
            .weak(),
        );
        ui.add_space(6.0);

        egui::Grid::new("settings_blackbox").show(ui, |ui| {
            // Master switch binds to the live `enabled` field (what sync reads)
            // and mirrors into the persisted settings.
            ui.checkbox(&mut self.blackbox.enabled, "Enable blackbox recorder");
            self.blackbox.settings.enabled = self.blackbox.enabled;
            ui.end_row();

            ui.label("Encoder:");
            egui::ComboBox::from_id_salt("blackbox_encoder")
                .selected_text(&self.blackbox.settings.encoder)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.blackbox.settings.encoder,
                        "libx264".to_string(),
                        "libx264 (software)",
                    );
                    ui.selectable_value(
                        &mut self.blackbox.settings.encoder,
                        "h264_nvenc".to_string(),
                        "h264_nvenc (NVIDIA hardware)",
                    );
                });
            ui.end_row();

            ui.label("Output directory:");
            ui.horizontal(|ui| {
                let display = if self.blackbox.settings.output_dir.is_empty() {
                    "(recording path / Blackbox)".to_string()
                } else {
                    self.blackbox.settings.output_dir.clone()
                };
                ui.label(display);
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_directory(
                            std::env::var("USERPROFILE")
                                .unwrap_or_else(|_| "C:\\Users".to_string()),
                        )
                        .pick_folder()
                    {
                        self.blackbox.settings.output_dir =
                            path.to_string_lossy().into_owned();
                    }
                }
            });
            ui.end_row();

            ui.label("Segment length:");
            ui.add(
                egui::Slider::new(&mut self.blackbox.settings.segment_duration_secs, 60..=3600)
                    .suffix(" s"),
            );
            ui.end_row();

            ui.label("Segment size cap:");
            ui.add(
                egui::Slider::new(&mut self.blackbox.settings.segment_size_mb, 64..=4096)
                    .suffix(" MiB"),
            );
            ui.end_row();

            ui.label("CRF (x264):");
            ui.add(egui::Slider::new(&mut self.blackbox.settings.crf, 0..=51));
            ui.end_row();

            ui.label("Bitrate (nvenc):");
            ui.add(
                egui::Slider::new(
                    &mut self.blackbox.settings.video_bitrate_kbps,
                    500..=50000,
                )
                .suffix(" kbps"),
            );
            ui.end_row();

            ui.label("Disk low warning:");
            ui.add(
                egui::Slider::new(&mut self.blackbox.settings.disk_low_warn_percent, 5..=50)
                    .suffix("%"),
            );
            ui.end_row();

            ui.label("Disk critical pause:");
            ui.add(
                egui::Slider::new(&mut self.blackbox.settings.disk_low_critical_percent, 1..=20)
                    .suffix("%"),
            );
            ui.end_row();

            ui.label("Max retention:");
            ui.add(
                egui::Slider::new(&mut self.blackbox.settings.max_retention_gb, 0..=200)
                    .suffix(" GiB (0 = unlimited)"),
            );
            ui.end_row();

            ui.label("Stall threshold:");
            ui.add(
                egui::Slider::new(&mut self.blackbox.settings.stall_threshold_secs, 1..=120)
                    .suffix(" s"),
            );
            ui.end_row();
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Status:").strong());
            let st = &self.blackbox.status;
            if !self.blackbox.enabled {
                ui.label("disabled");
            } else if !st.running {
                ui.label("idle (no capture source)");
            } else if st.disk_paused {
                ui.label(
                    egui::RichText::new("disk full — ingestion paused")
                        .color(egui::Color32::from_rgb(210, 160, 0)),
                );
            } else {
                ui.label(
                    egui::RichText::new("recording").color(egui::Color32::from_rgb(60, 200, 120)),
                );
            }

            let running = self
                .blackbox
                .engine
                .as_ref()
                .map(|e| e.is_running())
                .unwrap_or(false);
            if running && ui.button("Apply & restart recorder").clicked() {
                // Stop now; the next update() tick rebuilds the engine from the
                // (possibly edited) settings with a fresh segment.
                self.stop_blackbox();
            }
        });

        if let Some(e) = &self.blackbox.status.last_error {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Last error: {e}")).color(egui::Color32::from_rgb(200, 90, 90)));
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Most changes take effect on the next capture start. Toggling \
                 Enable applies immediately. Container is fixed to mkv for \
                 crash-safety.",
            )
            .small()
            .weak(),
        );
    }
}
