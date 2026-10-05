//! UI-thread glue for the Blackbox Dual Recording Engine.
//!
//! The engine itself ([`robs_outputs::BlackboxEngine`]) runs on its own std
//! threads and is intentionally decoupled from the UI loop. This module is
//! the thin adapter between the engine and the rest of the app:
//!
//! - [`BlackboxService`] owns the engine cluster ([`BlackboxState`]): it
//!   builds a [`BlackboxConfig`] from the user's settings + the active output
//!   dimensions, starts/stops the engine so it runs exactly while a capture
//!   source is active (and is enabled) — independent of the Record button /
//!   pause state, submits the captured frame each tick (strictly
//!   non-blocking), and drains the engine's event channel into the cached
//!   status snapshot. Notable discrete events come back as log lines for the
//!   facade to enter into the event log.
//! - The [`RobsController`] delegates at the bottom keep the exact call
//!   surface the tick loop, the recorder, and the view layer already use.
//!
//! Frame format note: capture sources (DXGI / GDI / webcam) yield BGRA, which
//! is exactly what ffmpeg consumes. The preview path swaps BGRA→RGBA for
//! view upload *after* this tap, so [`RobsController::submit_blackbox_frame`]
//! must be handed the raw BGRA buffer before that swap.

use std::path::PathBuf;
use std::sync::Arc;

use robs_core::event::{BlackboxEvent, RobsEvent};
use robs_outputs::{BlackboxConfig, BlackboxEngine, LocalFileSink};

use super::state::{BlackboxState, EventLogKind, OutputSpec};
use super::RobsController;

/// Service owning the Blackbox engine state cluster ([`BlackboxState`]): the
/// engine, its settings, the cached status snapshot, and the event channel.
/// Log lines are returned to the facade (the event log is facade state).
pub struct BlackboxService {
    state: BlackboxState,
}

impl BlackboxService {
    /// Build the service with a fresh event bus and the default settings,
    /// defaulting the encoder to the best available hardware/software.
    pub(crate) fn new(nvenc_available: bool) -> Self {
        let (bus, rx) = robs_core::EventBus::new();
        let mut settings = robs_profiles::settings::BlackboxSettings::default();
        if nvenc_available {
            settings.encoder = "h264_nvenc".into();
        } else {
            settings.encoder = "libx264".into();
        }
        Self {
            state: BlackboxState {
                enabled: settings.enabled,
                settings,
                engine: None,
                status: robs_core::event::BlackboxStatus::default(),
                event_tx: bus.tx(),
                event_rx: Some(rx),
                session_override: None,
            },
        }
    }

    /// Reconcile the engine's run state with the current capture situation.
    ///
    /// The engine should be running iff blackbox is enabled *and* the current
    /// scene has a visible capture source. Called once per frame from
    /// `tick()`, before frame processing, so the engine is guaranteed live
    /// when the per-frame tap fires. `spec` mirrors the facade's flat output
    /// fields and `recording_path` the recording destination, read at call
    /// time by the facade delegate.
    pub(crate) fn sync(
        &mut self,
        has_capture_source: bool,
        spec: OutputSpec,
        recording_path: &str,
    ) {
        let should_run = self.enabled && has_capture_source;
        let is_running = self
            .engine
            .as_ref()
            .map(|e| e.is_running())
            .unwrap_or(false);

        if should_run && !is_running {
            self.start(spec, recording_path);
        } else if !should_run && is_running {
            self.stop();
        }
    }

    /// Build a fresh engine from the current settings and start it. The engine
    /// is (re)created on every start so setting edits take effect on the next
    /// capture session without a live reconfigure path.
    fn start(&mut self, spec: OutputSpec, recording_path: &str) {
        let config = self.build_config(spec, recording_path);
        let sink = Arc::new(LocalFileSink::new(&config));
        let mut engine = BlackboxEngine::new(config, sink, self.event_tx.clone());
        engine.start();
        // Seed the UI snapshot from the engine's own view of itself.
        self.status = engine.status();
        self.engine = Some(engine);
    }

    pub(crate) fn stop(&mut self) {
        if let Some(mut engine) = self.engine.take() {
            engine.stop();
            self.status = engine.status();
        }
    }

    /// Project the user's blackbox settings (+ active output dims/fps) into the
    /// runtime config the engine consumes. An empty `output_dir` resolves to a
    /// `Blackbox/` subfolder of the recording path (or the home `Videos`/
    /// `Movies` folder when no recording path is set), matching the main
    /// recorder's convention.
    fn build_config(&self, spec: OutputSpec, recording_path: &str) -> BlackboxConfig {
        // A recording in progress redirects output into its session folder
        // (`<session>/Blackbox`, set in `record.rs`); otherwise the
        // settings/default resolution applies.
        let output_dir = if let Some(session_dir) = self.session_override.as_ref() {
            session_dir.clone()
        } else if self.settings.output_dir.is_empty() {
            let base = if recording_path.is_empty() {
                super::default_videos_dir()
            } else {
                recording_path.to_string()
            };
            PathBuf::from(base).join("Blackbox")
        } else {
            PathBuf::from(&self.settings.output_dir)
        };

        BlackboxConfig {
            output_dir,
            output_width: spec.width,
            output_height: spec.height,
            fps: spec.fps,
            segment_duration_secs: self.settings.segment_duration_secs,
            segment_size_mb: self.settings.segment_size_mb,
            encoder: self.settings.encoder.clone(),
            crf: self.settings.crf,
            video_bitrate_kbps: self.settings.video_bitrate_kbps,
            disk_low_warn_percent: self.settings.disk_low_warn_percent,
            disk_low_critical_percent: self.settings.disk_low_critical_percent,
            max_retention_gb: self.settings.max_retention_gb,
            stall_threshold_secs: self.settings.stall_threshold_secs,
        }
    }

    /// Forward a captured frame to the engine. Non-blocking: the engine drops
    /// the frame (and counts it) if its channel is full or the disk is
    /// critically full. Pass the raw **BGRA** buffer, before the preview path
    /// swaps it to RGBA. No-op when no engine is running.
    pub(crate) fn submit_frame(&self, data: &[u8], width: u32, height: u32) {
        if let Some(engine) = self.engine.as_ref() {
            engine.submit_frame(data, width, height);
        }
    }

    /// Drain all pending engine events into a Vec. Split out from
    /// [`Self::apply_events`] so the immutable borrow of `event_rx` ends
    /// before the facade logs.
    pub(crate) fn drain_events(&mut self) -> Vec<BlackboxEvent> {
        let Some(rx) = self.event_rx.as_ref() else {
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
    /// updates and turn the notable discrete events (start/stop, storage
    /// warnings, stalls, recovery, errors) into complete event-log lines,
    /// returned in arrival order for the facade to log. Per-segment events
    /// are logged at info level; with the default 15-min rotation that is
    /// not chatty.
    pub(crate) fn apply_events(
        &mut self,
        events: Vec<BlackboxEvent>,
    ) -> Vec<(String, EventLogKind)> {
        let mut logs = Vec::new();
        for ev in events {
            match ev {
                BlackboxEvent::StatusUpdated { status } => {
                    self.status = status;
                }
                BlackboxEvent::Started => {
                    self.status.running = true;
                    logs.push((
                        "Blackbox safety recorder started".to_string(),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Stopped => {
                    self.status.running = false;
                    logs.push((
                        "Blackbox safety recorder stopped".to_string(),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::SegmentStarted { path, index } => {
                    logs.push((
                        format!("Blackbox segment #{} started: {}", index, path),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::SegmentClosed {
                    index,
                    bytes,
                    duration_ms,
                    ..
                } => {
                    let secs = duration_ms / 1000;
                    let mib = bytes as f64 / 1_048_576.0;
                    logs.push((
                        format!(
                            "Blackbox segment #{} closed ({}, {:.1} MiB)",
                            index,
                            crate::RobsController::format_time(secs),
                            mib
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::StorageLow { free_percent, .. } => {
                    logs.push((
                        format!(
                            "Blackbox disk low: {:.0}% free remaining",
                            free_percent
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::StorageCritical { free_bytes, .. } => {
                    let gib = free_bytes as f64 / 1_073_741_824.0;
                    logs.push((
                        format!(
                            "Blackbox disk critical: pausing ingestion ({:.1} GiB free)",
                            gib
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Stalled { seconds_idle } => {
                    logs.push((
                        format!(
                            "Blackbox stalled: no frames for {}s",
                            seconds_idle
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Recovered { path } => {
                    logs.push((
                        format!("Blackbox recovered segment: {}", path),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Error { message } => {
                    self.status.last_error = Some(message.clone());
                    logs.push((
                        format!("Blackbox error: {}", message),
                        EventLogKind::Info,
                    ));
                }
            }
        }
        logs
    }
}

impl std::ops::Deref for BlackboxService {
    type Target = BlackboxState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl std::ops::DerefMut for BlackboxService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl RobsController {
    /// Reconcile the engine's run state with the current capture situation.
    /// Delegates to [`BlackboxService::sync`], passing the output geometry
    /// and recording path the service previously read off the controller.
    pub(crate) fn sync_blackbox_engine(&mut self, has_capture_source: bool) {
        let spec = OutputSpec {
            width: self.output_width,
            height: self.output_height,
            fps: self.fps_setting,
        };
        let recording_path = self.recording_path.clone();
        self.blackbox.sync(has_capture_source, spec, &recording_path);
    }

    pub fn stop_blackbox(&mut self) {
        self.blackbox.stop();
    }

    /// Forward a captured frame to the engine (see
    /// [`BlackboxService::submit_frame`]).
    pub(crate) fn submit_blackbox_frame(&self, data: &[u8], width: u32, height: u32) {
        self.blackbox.submit_frame(data, width, height);
    }

    /// Drain all pending engine events into a Vec.
    pub(crate) fn drain_blackbox_events(&mut self) -> Vec<BlackboxEvent> {
        self.blackbox.drain_events()
    }

    /// Apply drained events: refresh the service's status snapshot and log
    /// the notable discrete events, in arrival order.
    pub(crate) fn apply_blackbox_events(&mut self, events: Vec<BlackboxEvent>) {
        for (message, kind) in self.blackbox.apply_events(events) {
            self.log_event(message, kind);
        }
    }
}
