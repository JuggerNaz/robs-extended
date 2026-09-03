//! View half of the Blackbox Dual Recording Engine UI.
//!
//! The engine plumbing lives in `robs-controller` (see that crate's `blackbox`
//! module): it builds the config from the user's settings, starts/stops the
//! engine so it runs exactly while a capture source is active, taps captured
//! frames, and drains engine events into the cached status snapshot and the
//! event log. This module holds only the presentation piece: a compact health
//! chip for the bottom status strip.

use robs_controller::RobsController;

use super::RobsApp;

impl RobsApp {
    /// Compact health chip for the bottom status strip. Shown only when the
    /// engine is enabled and currently running (i.e. a capture source is
    /// active). Color encodes state: green while capturing, amber when disk
    /// ingestion is paused, grey when idle-but-running. A hover tooltip carries
    /// the full health summary (segments, bytes, duration, disk, drops, errors).
    pub(crate) fn blackbox_status_chip(&self, ui: &mut eframe::egui::Ui) {
        if !self.blackbox.enabled || !self.blackbox.status.running {
            return;
        }
        let st = &self.blackbox.status;
        let (dot, color, label) = if st.disk_paused || st.storage.critical {
            (
                "\u{23F8}",
                eframe::egui::Color32::from_rgb(210, 160, 0),
                "BB PAUSED",
            )
        } else if st.capturing {
            (
                "\u{25CF}",
                eframe::egui::Color32::from_rgb(60, 200, 120),
                "BB",
            )
        } else {
            (
                "\u{25CF}",
                eframe::egui::Color32::from_rgb(120, 120, 120),
                "BB",
            )
        };
        let resp = ui.label(eframe::egui::RichText::new(format!("{dot} {label}")).color(color).strong());

        let storage = if st.storage.total_bytes > 0 {
            format!("{:.0}% free", st.storage.free_percent)
        } else {
            "disk ?".to_string()
        };
        let mut tip = format!(
            "Blackbox safety recorder\nSegments written: {}  ({:.1} MiB)\nDuration: {}\nDisk: {}",
            st.segments_written,
            st.bytes_written as f64 / 1_048_576.0,
            RobsController::format_time(st.total_duration_ms / 1000),
            storage,
        );
        if st.dropped_frames > 0 {
            tip.push_str(&format!("\nDropped frames: {}", st.dropped_frames));
        }
        if st.disk_paused {
            tip.push_str("\nIngestion paused (disk critical)");
        }
        if let Some(e) = &st.last_error {
            tip.push_str(&format!("\nLast error: {e}"));
        }
        if let Some(p) = &st.current_segment_path {
            tip.push_str(&format!("\nCurrent segment: {p}"));
        }
        resp.on_hover_text(tip);
    }
}
