//! UI-thread glue for the Short Clip Anomaly Capture engine.
//!
//! The engine itself ([`robs_outputs::AnomalyCaptureEngine`]) runs on its own
//! std threads and is decoupled from the egui loop. This thin adapter on
//! [`RobsApp`]:
//!
//! - builds an [`AnomalyConfig`] from the user's settings + active output dims,
//! - starts/stops the engine on explicit user action (Start/Stop Buffer),
//! - submits the captured frame each tick (strictly non-blocking),
//! - triggers clip capture on demand (Capture Clip button / programmatic),
//! - drains the engine's event channel into the cached status + event log.
//!
//! Like the Blackbox tap, frames are handed in as raw **BGRA** before the
//! preview path swaps them to RGBA.

use std::path::PathBuf;

use robs_core::event::{AnomalyEvent, RobsEvent};
use robs_outputs::{AnomalyCaptureEngine, AnomalyConfig};

use super::state::EventLogKind;
use super::RobsApp;

impl RobsApp {
    /// Build a fresh engine from the current settings and start it. The engine
    /// is (re)created on every start so setting edits take effect immediately.
    pub(crate) fn start_anomaly(&mut self) {
        let config = self.build_anomaly_config();
        let mut engine = AnomalyCaptureEngine::new(config, self.anomaly.event_tx.clone());
        engine.start();
        self.anomaly.status = engine.status();
        self.anomaly.enabled = true;
        self.anomaly.engine = Some(engine);
    }

    pub(crate) fn stop_anomaly(&mut self) {
        if let Some(mut engine) = self.anomaly.engine.take() {
            engine.stop();
            self.anomaly.status = engine.status();
        }
        self.anomaly.enabled = false;
    }

    /// Persist the anomaly settings to the config-dir `settings.json`.
    /// Called when the Settings window closes (the app's commit gesture —
    /// eframe runs without the `persistence` feature, so there is no exit
    /// hook). Silent on success; failures surface in the event log.
    pub(crate) fn save_anomaly_settings(&mut self) {
        if let Err(e) = self.anomaly.settings.save() {
            self.log_event(
                format!("Failed to save anomaly settings: {e}"),
                EventLogKind::Info,
            );
        }
    }

    /// Project the user's anomaly settings (+ active output dims/fps) into the
    /// runtime config. An empty `output_dir` resolves to an `Anomaly/` subfolder
    /// of the recording path (or the home `Videos`/`Movies` folder when none
    /// is set).
    fn build_anomaly_config(&self) -> AnomalyConfig {
        let output_dir = if self.anomaly.settings.output_dir.is_empty() {
            let base = if self.recording_path.is_empty() {
                super::default_videos_dir()
            } else {
                self.recording_path.clone()
            };
            PathBuf::from(base).join("Anomaly")
        } else {
            PathBuf::from(&self.anomaly.settings.output_dir)
        };

        AnomalyConfig {
            output_dir,
            output_width: self.anomaly.settings.output_width,
            output_height: self.anomaly.settings.output_height,
            fps: self.fps_setting,
            pre_roll_secs: self.anomaly.settings.pre_roll_secs as u64,
            post_roll_secs: self.anomaly.settings.post_roll_secs as u64,
            max_buffer_bytes: self.anomaly.settings.max_buffer_mb as u64 * 1024 * 1024,
            segment_duration_secs: self.anomaly.settings.segment_duration_secs as u64,
            encoder: self.anomaly.settings.encoder.clone(),
            crf: self.anomaly.settings.crf,
            video_bitrate_kbps: self.anomaly.settings.video_bitrate_kbps,
            clip_prefix: self.anomaly.settings.clip_prefix.clone(),
            clip_suffix: self.anomaly.settings.clip_suffix.clone(),
            disk_low_warn_percent: 10,
        }
    }

    /// Forward a captured frame to the engine. Non-blocking; no-op without a
    /// running engine. Pass the raw **BGRA** buffer, before the preview swap.
    pub(crate) fn submit_anomaly_frame(&self, data: &[u8], width: u32, height: u32) {
        if let Some(engine) = self.anomaly.engine.as_ref() {
            engine.submit_frame(data, width, height);
        }
    }

    /// Manual Capture Clip trigger (button / future hotkey).
    pub(crate) fn request_anomaly_clip(&mut self) {
        let pre = self.anomaly.settings.pre_roll_secs as u64;
        let post = self.anomaly.settings.post_roll_secs as u64;
        if let Some(engine) = self.anomaly.engine.as_ref() {
            if engine.save(pre, post).is_some() {
                self.log_event(
                    format!(
                        "Anomaly clip requested (pre {}s / post {}s)",
                        pre, post
                    ),
                    EventLogKind::Info,
                );
            }
        } else {
            self.log_event(
                "Anomaly buffer not running — start it first",
                EventLogKind::Info,
            );
        }
    }

    /// Drain all pending engine events into a Vec (split out from
    /// [`Self::apply_anomaly_events`] so the immutable `event_rx` borrow ends
    /// before the `&mut self` log calls).
    pub(crate) fn drain_anomaly_events(&mut self) -> Vec<AnomalyEvent> {
        let Some(rx) = self.anomaly.event_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let RobsEvent::Anomaly(a) = ev {
                out.push(a);
            }
        }
        out
    }

    /// Apply drained events: refresh the status snapshot on periodic updates and
    /// log the discrete lifecycle / clip events.
    pub(crate) fn apply_anomaly_events(&mut self, events: Vec<AnomalyEvent>) {
        for ev in events {
            match ev {
                AnomalyEvent::StatusUpdated { status } => {
                    self.anomaly.status = status;
                }
                AnomalyEvent::Started => {
                    self.anomaly.status.running = true;
                    self.log_event("Anomaly buffer started", EventLogKind::Info);
                }
                AnomalyEvent::Stopped => {
                    self.anomaly.status.running = false;
                    self.log_event("Anomaly buffer stopped", EventLogKind::Info);
                }
                AnomalyEvent::BufferReady { secs_filled } => {
                    self.log_event(
                        format!("Anomaly buffer ready ({}s held)", secs_filled),
                        EventLogKind::Info,
                    );
                }
                AnomalyEvent::ClipRequested { clip_id } => {
                    self.anomaly.status.clips_busy = 1;
                    self.log_event(
                        format!("Anomaly clip requested ({})", clip_id),
                        EventLogKind::Info,
                    );
                }
                AnomalyEvent::ClipReady { clip_id, path } => {
                    self.anomaly.status.clips_busy = 0;
                    self.log_event(
                        format!("Anomaly clip saved: {} ({})", path, clip_id),
                        EventLogKind::Info,
                    );
                }
                AnomalyEvent::ClipFailed { clip_id, message } => {
                    self.anomaly.status.clips_busy = 0;
                    self.log_event(
                        format!("Anomaly clip failed ({}): {}", clip_id, message),
                        EventLogKind::Info,
                    );
                }
                AnomalyEvent::ClipBusy { clip_id } => {
                    self.log_event(
                        format!(
                            "Anomaly clip busy — export already in progress ({})",
                            clip_id
                        ),
                        EventLogKind::Info,
                    );
                }
                AnomalyEvent::Error { message } => {
                    self.anomaly.status.last_error = Some(message.clone());
                    self.log_event(format!("Anomaly error: {}", message), EventLogKind::Info);
                }
            }
        }
    }

    /// Status chip + Start/Stop buffer + Capture Clip controls for the bottom
    /// status strip. Always rendered (compact); the Capture Clip button is
    /// disabled unless the buffer is running and actively receiving frames.
    pub(crate) fn anomaly_controls(&mut self, ui: &mut eframe::egui::Ui) {
        let running = self
            .anomaly
            .engine
            .as_ref()
            .map(|e| e.is_running())
            .unwrap_or(false);
        let exporting = self.anomaly.status.clips_busy > 0;

        // Start/Stop buffer toggle.
        let toggle_label = if running { "■ Stop Buffer" } else { "● Start Buffer" };
        if ui.button(toggle_label).clicked() {
            if running {
                self.stop_anomaly();
            } else {
                self.start_anomaly();
            }
        }

        // Capture Clip (enabled only while buffering and not already exporting).
        let can_capture = running && self.anomaly.status.buffering && !exporting;
        if ui
            .add_enabled(can_capture, eframe::egui::Button::new("⏺ Capture Clip"))
            .clicked()
        {
            self.request_anomaly_clip();
        }

        // Compact health chip while running.
        if running {
            let st = &self.anomaly.status;
            let (dot, color, label) = if exporting {
                (
                    "\u{23F3}",
                    eframe::egui::Color32::from_rgb(210, 160, 0),
                    "CLIP",
                )
            } else if st.buffering {
                (
                    "\u{25CF}",
                    eframe::egui::Color32::from_rgb(60, 200, 120),
                    "BUF",
                )
            } else {
                (
                    "\u{25CF}",
                    eframe::egui::Color32::from_rgb(120, 120, 120),
                    "BUF",
                )
            };
            let resp = ui.label(
                eframe::egui::RichText::new(format!("{} {} {}s", dot, label, st.buffer_secs_filled))
                    .color(color)
                    .strong(),
            );

            let storage = if st.storage.total_bytes > 0 {
                format!("{:.0}% free", st.storage.free_percent)
            } else {
                "disk ?".to_string()
            };
            let mut tip = format!(
                "Anomaly clip buffer\nBuffered: {}s (pre-roll {}s, post-roll {}s)\nClips exported: {}\nDisk: {}",
                st.buffer_secs_filled,
                self.anomaly.settings.pre_roll_secs,
                self.anomaly.settings.post_roll_secs,
                st.clips_exported,
                storage,
            );
            if st.dropped_frames > 0 {
                tip.push_str(&format!("\nDropped frames: {}", st.dropped_frames));
            }
            if let Some(e) = &st.last_error {
                tip.push_str(&format!("\nLast error: {e}"));
            }
            if let Some(p) = &st.last_clip_path {
                tip.push_str(&format!("\nLast clip: {p}"));
            }
            resp.on_hover_text(tip);
        }
    }
}
