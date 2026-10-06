//! UI-thread glue for the Short Clip Anomaly Capture engine.
//!
//! The engine itself ([`robs_outputs::AnomalyCaptureEngine`]) runs on its own
//! std threads and is decoupled from the UI loop. This module is the thin
//! adapter between the engine and the rest of the app:
//!
//! - [`AnomalyService`] owns the engine cluster ([`AnomalyState`]): it builds
//!   an [`AnomalyConfig`] from the user's settings + active output fps,
//!   starts/stops the engine on explicit user action (Start/Stop Buffer),
//!   submits the captured frame each tick (strictly non-blocking), triggers
//!   clip capture on demand (Capture Clip button / programmatic), and drains
//!   the engine's event channel into the cached status. Discrete lifecycle /
//!   clip events come back as log lines for the facade.
//! - The [`RobsController`] delegates at the bottom keep the exact call
//!   surface the tick loop, the recorder, and the view layer already use.
//!
//! Like the Blackbox tap, frames are handed in as raw **BGRA** before the
//! preview path swaps them to RGBA.

use std::path::PathBuf;

use robs_core::event::{AnomalyEvent, RobsEvent};
use robs_outputs::{AnomalyCaptureEngine, AnomalyConfig};

use super::state::{AnomalyState, EventLogKind, OutputSpec};
use super::RobsController;

/// Service owning the Short Clip Anomaly Capture state cluster
/// ([`AnomalyState`]): the rolling-buffer engine, its settings, the cached
/// status snapshot, and the event channel. Log lines are returned to the
/// facade (the event log is facade state).
pub struct AnomalyService {
    state: AnomalyState,
}

impl AnomalyService {
    /// Build the service with a fresh event bus and the persisted settings
    /// (config dir `settings.json`; defaults on first run or an unreadable
    /// file). The buffer starts stopped; the user toggles it explicitly.
    pub(crate) fn new() -> Self {
        let (bus, rx) = robs_core::EventBus::new();
        Self {
            state: AnomalyState {
                enabled: false,
                settings: robs_profiles::settings::AnomalySettings::load_or_default(),
                engine: None,
                status: robs_core::event::AnomalyStatus::default(),
                event_tx: bus.tx(),
                event_rx: Some(rx),
                session_override: None,
            },
        }
    }

    /// Build a fresh engine from the current settings and start it. The engine
    /// is (re)created on every start so setting edits take effect immediately.
    /// `spec` mirrors the facade's flat fps setting and `recording_path` the
    /// recording destination, read at call time by the facade delegate.
    pub(crate) fn start(&mut self, spec: OutputSpec, recording_path: &str) {
        let config = self.build_config(spec, recording_path);
        let mut engine = AnomalyCaptureEngine::new(config, self.event_tx.clone());
        engine.start();
        self.status = engine.status();
        self.enabled = true;
        self.engine = Some(engine);
    }

    pub(crate) fn stop(&mut self) {
        if let Some(mut engine) = self.engine.take() {
            engine.stop();
            self.status = engine.status();
        }
        self.enabled = false;
    }

    /// Persist the anomaly settings to the config-dir `settings.json`.
    /// Called when the Settings window closes (the app's commit gesture —
    /// the runtime does not persist state, so there is no exit hook).
    /// Returns the event-log line on failure, `None` on success (silent).
    pub(crate) fn save_settings(&mut self) -> Option<String> {
        if let Err(e) = self.settings.save() {
            return Some(format!("Failed to save anomaly settings: {e}"));
        }
        None
    }

    /// Project the user's anomaly settings (+ active output fps) into the
    /// runtime config. An empty `output_dir` resolves to an `Anomaly/`
    /// subfolder of the recording path (or the home `Videos`/`Movies` folder
    /// when none is set).
    fn build_config(&self, spec: OutputSpec, recording_path: &str) -> AnomalyConfig {
        // A recording in progress redirects clips into its session folder
        // (`<session>/Anomaly`, set in `record.rs`); otherwise the
        // settings/default resolution applies.
        let output_dir = if let Some(session_dir) = self.session_override.as_ref() {
            session_dir.clone()
        } else if self.settings.output_dir.is_empty() {
            let base = if recording_path.is_empty() {
                super::default_videos_dir()
            } else {
                recording_path.to_string()
            };
            PathBuf::from(base).join("Anomaly")
        } else {
            PathBuf::from(&self.settings.output_dir)
        };

        AnomalyConfig {
            output_dir,
            output_width: self.settings.output_width,
            output_height: self.settings.output_height,
            fps: spec.fps,
            pre_roll_secs: self.settings.pre_roll_secs as u64,
            post_roll_secs: self.settings.post_roll_secs as u64,
            max_buffer_bytes: self.settings.max_buffer_mb as u64 * 1024 * 1024,
            segment_duration_secs: self.settings.segment_duration_secs as u64,
            encoder: self.settings.encoder.clone(),
            crf: self.settings.crf,
            video_bitrate_kbps: self.settings.video_bitrate_kbps,
            clip_prefix: self.settings.clip_prefix.clone(),
            clip_suffix: self.settings.clip_suffix.clone(),
            disk_low_warn_percent: 10,
        }
    }

    /// Forward a captured frame to the engine. Non-blocking; no-op without a
    /// running engine. Pass the raw **BGRA** buffer, before the preview swap.
    pub(crate) fn submit_frame(&self, data: &[u8], width: u32, height: u32) {
        if let Some(engine) = self.engine.as_ref() {
            engine.submit_frame(data, width, height);
        }
    }

    /// Manual Capture Clip trigger (button / future hotkey). Returns the
    /// event-log line — logged when a clip was requested or when the buffer
    /// is not running; silent when the engine declined the save (already
    /// exporting).
    pub(crate) fn request_clip(&mut self) -> Option<String> {
        let pre = self.settings.pre_roll_secs as u64;
        let post = self.settings.post_roll_secs as u64;
        if let Some(engine) = self.engine.as_ref() {
            if engine.save(pre, post).is_some() {
                return Some(format!(
                    "Anomaly clip requested (pre {}s / post {}s)",
                    pre, post
                ));
            }
            None
        } else {
            Some("Anomaly buffer not running — start it first".to_string())
        }
    }

    /// Drain all pending engine events into a Vec (split out from
    /// [`Self::apply_events`] so the immutable `event_rx` borrow ends before
    /// the facade logs).
    pub(crate) fn drain_events(&mut self) -> Vec<AnomalyEvent> {
        let Some(rx) = self.event_rx.as_ref() else {
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

    /// Apply drained events: refresh the status snapshot on periodic updates
    /// and turn the discrete lifecycle / clip events into complete event-log
    /// lines, returned in arrival order for the facade to log.
    pub(crate) fn apply_events(
        &mut self,
        events: Vec<AnomalyEvent>,
    ) -> Vec<(String, EventLogKind)> {
        let mut logs = Vec::new();
        for ev in events {
            match ev {
                AnomalyEvent::StatusUpdated { status } => {
                    self.status = status;
                }
                AnomalyEvent::Started => {
                    self.status.running = true;
                    logs.push(("Anomaly buffer started".to_string(), EventLogKind::Info));
                }
                AnomalyEvent::Stopped => {
                    self.status.running = false;
                    logs.push(("Anomaly buffer stopped".to_string(), EventLogKind::Info));
                }
                AnomalyEvent::BufferReady { secs_filled } => {
                    logs.push((
                        format!("Anomaly buffer ready ({}s held)", secs_filled),
                        EventLogKind::Info,
                    ));
                }
                AnomalyEvent::ClipRequested { clip_id } => {
                    self.status.clips_busy = 1;
                    logs.push((
                        format!("Anomaly clip requested ({})", clip_id),
                        EventLogKind::Info,
                    ));
                }
                AnomalyEvent::ClipReady { clip_id, path } => {
                    self.status.clips_busy = 0;
                    logs.push((
                        format!("Anomaly clip saved: {} ({})", path, clip_id),
                        EventLogKind::Info,
                    ));
                }
                AnomalyEvent::ClipFailed { clip_id, message } => {
                    self.status.clips_busy = 0;
                    logs.push((
                        format!("Anomaly clip failed ({}): {}", clip_id, message),
                        EventLogKind::Info,
                    ));
                }
                AnomalyEvent::ClipBusy { clip_id } => {
                    logs.push((
                        format!(
                            "Anomaly clip busy — export already in progress ({})",
                            clip_id
                        ),
                        EventLogKind::Info,
                    ));
                }
                AnomalyEvent::Error { message } => {
                    self.status.last_error = Some(message.clone());
                    logs.push((
                        format!("Anomaly error: {}", message),
                        EventLogKind::Info,
                    ));
                }
            }
        }
        logs
    }
}

impl std::ops::Deref for AnomalyService {
    type Target = AnomalyState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl std::ops::DerefMut for AnomalyService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl RobsController {
    /// Build a fresh engine from the current settings and start it (see
    /// [`AnomalyService::start`]).
    pub fn start_anomaly(&mut self) {
        let spec = OutputSpec {
            width: self.output_width,
            height: self.output_height,
            fps: self.fps_setting,
        };
        let recording_path = self.recording_path.clone();
        self.anomaly.start(spec, &recording_path);
    }

    pub fn stop_anomaly(&mut self) {
        self.anomaly.stop();
    }

    /// Persist the anomaly settings to the config-dir `settings.json`.
    /// Silent on success; failures surface in the event log.
    pub fn save_anomaly_settings(&mut self) {
        if let Some(message) = self.anomaly.save_settings() {
            self.log_event(message, EventLogKind::Info);
        }
    }

    /// Manual Capture Clip trigger (see [`AnomalyService::request_clip`]).
    pub fn request_anomaly_clip(&mut self) {
        if let Some(message) = self.anomaly.request_clip() {
            self.log_event(message, EventLogKind::Info);
        }
    }

    /// Forward a captured frame to the engine (see
    /// [`AnomalyService::submit_frame`]).
    pub(crate) fn submit_anomaly_frame(&self, data: &[u8], width: u32, height: u32) {
        self.anomaly.submit_frame(data, width, height);
    }

    /// Drain all pending engine events into a Vec.
    pub(crate) fn drain_anomaly_events(&mut self) -> Vec<AnomalyEvent> {
        self.anomaly.drain_events()
    }

    /// Apply drained events: refresh the service's status snapshot and log
    /// the discrete lifecycle / clip events, in arrival order.
    pub(crate) fn apply_anomaly_events(&mut self, events: Vec<AnomalyEvent>) {
        for (message, kind) in self.anomaly.apply_events(events) {
            self.log_event(message, kind);
        }
    }
}
