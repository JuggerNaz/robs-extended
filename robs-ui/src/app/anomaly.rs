//! View half of the Short Clip Anomaly Capture UI.
//!
//! The engine plumbing lives in `robs-controller` (see that crate's `anomaly`
//! module): it builds the config from the user's settings, starts/stops the
//! engine, submits captured frames, triggers clip capture, and drains engine
//! events into the cached status and event log. This module holds only the
//! bottom-status-strip controls: Start/Stop buffer, Capture Clip, and the
//! compact status chip.

use super::RobsApp;

impl RobsApp {
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
