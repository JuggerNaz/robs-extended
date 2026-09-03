//! UI-thread glue for the Blackbox Dual Recording Engine.
//!
//! The engine itself ([`robs_outputs::BlackboxEngine`]) runs on its own std
//! threads and is intentionally decoupled from the UI loop. This module is
//! the thin adapter on [`RobsController`] that:
//!
//! - builds a [`BlackboxConfig`] from the user's settings + the active output
//!   dimensions,
//! - starts/stops the engine so it runs exactly while a capture source is
//!   active (and is enabled) — independent of the Record button / pause state,
//! - submits the captured frame each tick (strictly non-blocking),
//! - drains the engine's event channel into the cached status snapshot and the
//!   event log for UI display.
//!
//! Frame format note: capture sources (DXGI / GDI / webcam) yield BGRA, which
//! is exactly what ffmpeg consumes. The preview path swaps BGRA→RGBA for
//! view upload *after* this tap, so [`RobsController::submit_blackbox_frame`]
//! must be handed the raw BGRA buffer before that swap.

use std::path::PathBuf;
use std::sync::Arc;

use robs_core::event::{BlackboxEvent, RobsEvent};
use robs_outputs::{BlackboxConfig, BlackboxEngine, LocalFileSink};

use super::state::EventLogKind;
use super::RobsController;

impl RobsController {
    /// Reconcile the engine's run state with the current capture situation.
    ///
    /// The engine should be running iff blackbox is enabled *and* the current
    /// scene has a visible capture source. Called once per frame from
    /// `tick()`, before frame processing, so the engine is guaranteed live
    /// when the per-frame tap fires.
    pub(crate) fn sync_blackbox_engine(&mut self, has_capture_source: bool) {
        let should_run = self.blackbox.enabled && has_capture_source;
        let is_running = self
            .blackbox
            .engine
            .as_ref()
            .map(|e| e.is_running())
            .unwrap_or(false);

        if should_run && !is_running {
            self.start_blackbox();
        } else if !should_run && is_running {
            self.stop_blackbox();
        }
    }

    /// Build a fresh engine from the current settings and start it. The engine
    /// is (re)created on every start so setting edits take effect on the next
    /// capture session without a live reconfigure path.
    fn start_blackbox(&mut self) {
        let config = self.build_blackbox_config();
        let sink = Arc::new(LocalFileSink::new(&config));
        let mut engine = BlackboxEngine::new(config, sink, self.blackbox.event_tx.clone());
        engine.start();
        // Seed the UI snapshot from the engine's own view of itself.
        self.blackbox.status = engine.status();
        self.blackbox.engine = Some(engine);
    }

    pub fn stop_blackbox(&mut self) {
        if let Some(mut engine) = self.blackbox.engine.take() {
            engine.stop();
            self.blackbox.status = engine.status();
        }
    }

    /// Project the user's blackbox settings (+ active output dims/fps) into the
    /// runtime config the engine consumes. An empty `output_dir` resolves to a
    /// `Blackbox/` subfolder of the recording path (or the home `Videos`/
    /// `Movies` folder when no recording path is set), matching the main
    /// recorder's convention.
    fn build_blackbox_config(&self) -> BlackboxConfig {
        let output_dir = if self.blackbox.settings.output_dir.is_empty() {
            let base = if self.recording_path.is_empty() {
                super::default_videos_dir()
            } else {
                self.recording_path.clone()
            };
            PathBuf::from(base).join("Blackbox")
        } else {
            PathBuf::from(&self.blackbox.settings.output_dir)
        };

        BlackboxConfig {
            output_dir,
            output_width: self.output_width,
            output_height: self.output_height,
            fps: self.fps_setting,
            segment_duration_secs: self.blackbox.settings.segment_duration_secs,
            segment_size_mb: self.blackbox.settings.segment_size_mb,
            encoder: self.blackbox.settings.encoder.clone(),
            crf: self.blackbox.settings.crf,
            video_bitrate_kbps: self.blackbox.settings.video_bitrate_kbps,
            disk_low_warn_percent: self.blackbox.settings.disk_low_warn_percent,
            disk_low_critical_percent: self.blackbox.settings.disk_low_critical_percent,
            max_retention_gb: self.blackbox.settings.max_retention_gb,
            stall_threshold_secs: self.blackbox.settings.stall_threshold_secs,
        }
    }

    /// Forward a captured frame to the engine. Non-blocking: the engine drops
    /// the frame (and counts it) if its channel is full or the disk is
    /// critically full. Pass the raw **BGRA** buffer, before the preview path
    /// swaps it to RGBA. No-op when no engine is running.
    pub(crate) fn submit_blackbox_frame(&self, data: &[u8], width: u32, height: u32) {
        if let Some(engine) = self.blackbox.engine.as_ref() {
            engine.submit_frame(data, width, height);
        }
    }

    /// Drain all pending engine events into a Vec. Split out from
    /// [`Self::apply_blackbox_events`] so the immutable borrow of `event_rx`
    /// ends before we take `&mut self` to log.
    pub(crate) fn drain_blackbox_events(&mut self) -> Vec<BlackboxEvent> {
        let Some(rx) = self.blackbox.event_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let RobsEvent::Blackbox(b) = ev {
                out.push(b);
            }
        }
        out
    }

    /// Apply drained events: refresh the cached status snapshot on periodic
    /// updates and log the notable discrete events (start/stop, storage
    /// warnings, stalls, recovery, errors). Per-segment events are logged at
    /// info level; with the default 15-min rotation that is not chatty.
    pub(crate) fn apply_blackbox_events(&mut self, events: Vec<BlackboxEvent>) {
        for ev in events {
            match ev {
                BlackboxEvent::StatusUpdated { status } => {
                    self.blackbox.status = status;
                }
                BlackboxEvent::Started => {
                    self.blackbox.status.running = true;
                    self.log_event(
                        "Blackbox safety recorder started",
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::Stopped => {
                    self.blackbox.status.running = false;
                    self.log_event(
                        "Blackbox safety recorder stopped",
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::SegmentStarted { path, index } => {
                    self.log_event(
                        format!("Blackbox segment #{} started: {}", index, path),
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::SegmentClosed {
                    index,
                    bytes,
                    duration_ms,
                    ..
                } => {
                    let secs = duration_ms / 1000;
                    let mib = bytes as f64 / 1_048_576.0;
                    self.log_event(
                        format!(
                            "Blackbox segment #{} closed ({}, {:.1} MiB)",
                            index,
                            Self::format_time(secs),
                            mib
                        ),
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::StorageLow { free_percent, .. } => {
                    self.log_event(
                        format!(
                            "Blackbox disk low: {:.0}% free remaining",
                            free_percent
                        ),
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::StorageCritical { free_bytes, .. } => {
                    let gib = free_bytes as f64 / 1_073_741_824.0;
                    self.log_event(
                        format!(
                            "Blackbox disk critical: pausing ingestion ({:.1} GiB free)",
                            gib
                        ),
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::Stalled { seconds_idle } => {
                    self.log_event(
                        format!(
                            "Blackbox stalled: no frames for {}s",
                            seconds_idle
                        ),
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::Recovered { path } => {
                    self.log_event(
                        format!("Blackbox recovered segment: {}", path),
                        EventLogKind::Info,
                    );
                }
                BlackboxEvent::Error { message } => {
                    self.blackbox.status.last_error = Some(message.clone());
                    self.log_event(
                        format!("Blackbox error: {}", message),
                        EventLogKind::Info,
                    );
                }
            }
        }
    }
}
