//! UI-thread glue for the Blackbox Dual Recording Engine — MULTI-CHANNEL.
//!
//! The engine itself ([`robs_outputs::BlackboxEngine`]) runs on its own std
//! threads and is intentionally decoupled from the UI loop. This module is
//! the thin adapter between the engines and the rest of the app:
//!
//! - [`BlackboxService`] owns one [`BlackboxChannel`] per scene with a
//!   visible capture source (N channels = N independent engine instances —
//!   the engine is fully self-contained, so multi-channel needs no changes
//!   to `robs-outputs`). Each channel records to its own per-camera folder:
//!   `<session>/Blackbox/<Scene>` during a recording, otherwise
//!   `<base>/Blackbox/<Scene>`. `sync` runs once per tick from `tick()`,
//!   starting/stopping channels so each one runs exactly while its scene
//!   has a visible capture source (and blackbox is enabled) — independent
//!   of the Record button / pause state.
//! - Session redirects: at recording start `record.rs` calls
//!   [`BlackboxService::begin_session`] with `<session>/Blackbox`; the
//!   running channels are then stopped by the recorder so the next tick
//!   restarts them inside the session folder. When a session has a tape
//!   number, each channel that starts under it also writes a static
//!   `tape.json` manifest — the marker `merge.rs` discovers tapes by.
//! - The [`RobsController`] delegates at the bottom keep the call surface
//!   the tick loop, the recorder, and the view layer use.
//!
//! Frame format note: capture sources (DXGI / GDI / webcam) yield BGRA,
//! which is exactly what ffmpeg consumes. The preview path swaps
//! BGRA→RGBA for view upload *after* this tap, so
//! [`RobsController::submit_blackbox_frame`] must be handed the raw BGRA
//! buffer before that swap.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use robs_core::event::{BlackboxEvent, RobsEvent};
use robs_outputs::{BlackboxConfig, BlackboxEngine, LocalFileSink};

use super::merge;
use super::state::{BlackboxChannel, EventLogKind, OutputSpec};
use super::RobsController;

/// Service owning the blackbox channel cluster: shared settings + session
/// redirect, one [`BlackboxChannel`] per recording scene. Log lines are
/// returned to the facade (the event log is facade state).
pub struct BlackboxService {
    /// Master on/off. Honored live: toggling off stops running engines.
    pub enabled: bool,
    /// Editable settings; projected into a fresh `BlackboxConfig` whenever an
    /// engine (re)starts, so most edits take effect on the next start.
    pub settings: robs_profiles::settings::BlackboxSettings,
    /// When `Some`, channel output goes into the active recording's session
    /// folder (`<session>/Blackbox/<Scene>`) instead of the settings/default
    /// location. Set for the lifetime of a recording, cleared on stop.
    pub(crate) session_base: Option<PathBuf>,
    /// One channel per scene that has (had) a visible capture source.
    pub(crate) channels: Vec<BlackboxChannel>,
}

impl BlackboxService {
    /// Build the service with default settings, defaulting the encoder to the
    /// best available hardware/software. Channels are created lazily by
    /// [`Self::sync`] as scenes with capture sources appear.
    pub(crate) fn new(nvenc_available: bool) -> Self {
        let mut settings = robs_profiles::settings::BlackboxSettings::default();
        if nvenc_available {
            settings.encoder = "h264_nvenc".into();
        } else {
            settings.encoder = "libx264".into();
        }
        Self {
            enabled: settings.enabled,
            settings,
            session_base: None,
            channels: Vec::new(),
        }
    }

    // -- channel plumbing ---------------------------------------------------

    fn channel(&self, scene: &str) -> Option<&BlackboxChannel> {
        self.channels.iter().find(|c| c.scene == scene)
    }

    fn channel_mut(&mut self, scene: &str) -> Option<&mut BlackboxChannel> {
        self.channels.iter_mut().find(|c| c.scene == scene)
    }

    fn channel_running(&self, scene: &str) -> bool {
        self.channel(scene)
            .and_then(|c| c.engine.as_ref())
            .is_some_and(|e| e.is_running())
    }

    /// Resolve a channel's output directory (pure so it can be unit-tested):
    /// the session redirect wins, else the settings dir or
    /// `<recording path>/Blackbox`, always with the scene name appended —
    /// one folder per camera.
    fn channel_output_dir(
        settings_dir: &str,
        session_base: Option<&Path>,
        recording_path: &str,
        scene: &str,
    ) -> PathBuf {
        if let Some(base) = session_base {
            base.join(scene)
        } else if settings_dir.is_empty() {
            let base = if recording_path.is_empty() {
                super::default_videos_dir()
            } else {
                recording_path.to_string()
            };
            PathBuf::from(base).join("Blackbox").join(scene)
        } else {
            PathBuf::from(settings_dir).join(scene)
        }
    }

    /// Project the user's blackbox settings (+ active output dims/fps) into
    /// the runtime config `scene`'s engine consumes.
    fn build_config(&self, scene: &str, spec: &OutputSpec, recording_path: &str) -> BlackboxConfig {
        BlackboxConfig {
            output_dir: Self::channel_output_dir(
                &self.settings.output_dir,
                self.session_base.as_deref(),
                recording_path,
                scene,
            ),
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

    /// Ensure a channel entry exists for `scene` (with its own event bus).
    fn ensure_channel(&mut self, scene: &str) -> &mut BlackboxChannel {
        if self.channel(scene).is_none() {
            let (bus, rx) = robs_core::EventBus::new();
            self.channels.push(BlackboxChannel {
                scene: scene.to_string(),
                engine: None,
                status: robs_core::event::BlackboxStatus::default(),
                event_tx: bus.tx(),
                event_rx: Some(rx),
            });
        }
        self.channel_mut(scene).expect("just inserted")
    }

    /// Build a fresh engine for `scene` from the current settings and start
    /// it. The engine is (re)created on every start so setting edits take
    /// effect on the next capture session without a live reconfigure path.
    /// When the start happens under a taped recording session, the channel
    /// folder also receives its static `tape.json` manifest (written once —
    /// restarts within the same session keep the original).
    fn start_channel(
        &mut self,
        scene: &str,
        spec: &OutputSpec,
        recording_path: &str,
        tape_no: &str,
    ) {
        let config = self.build_config(scene, spec, recording_path);
        let sink = Arc::new(LocalFileSink::new(&config));
        let channel = self.ensure_channel(scene);
        let mut engine = BlackboxEngine::new(config, sink, channel.event_tx.clone());
        engine.start();
        // Seed the UI snapshot from the engine's own view of itself.
        channel.status = engine.status();
        channel.engine = Some(engine);

        if let (Some(base), false) = (self.session_base.as_ref(), tape_no.is_empty()) {
            let manifest = base.join(scene).join("tape.json");
            if !manifest.exists() {
                let session = base
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let text = merge::build_tape_manifest(
                    tape_no,
                    scene,
                    &session,
                    chrono::Local::now(),
                );
                if let Err(e) = std::fs::write(&manifest, text) {
                    eprintln!("[Blackbox] cannot write {}: {e}", manifest.display());
                }
            }
        }
    }

    fn stop_channel(&mut self, scene: &str) {
        if let Some(channel) = self.channel_mut(scene) {
            if let Some(mut engine) = channel.engine.take() {
                engine.stop();
                channel.status = engine.status();
            }
        }
    }

    // -- reconciliation -----------------------------------------------------

    /// Reconcile every channel's run state with the current capture
    /// situation. `scenes` lists each scene name with whether it currently
    /// has a visible capture source; a channel's engine should be running
    /// iff blackbox is enabled *and* its scene is in that list with `true`.
    /// Called once per frame from `tick()`, before frame processing, so the
    /// engines are guaranteed live when the per-frame taps fire.
    pub(crate) fn sync(
        &mut self,
        scenes: &[(String, bool)],
        spec: OutputSpec,
        recording_path: &str,
        tape_no: &str,
    ) {
        for (scene, has_source) in scenes {
            let running = self.channel_running(scene);
            if self.enabled && *has_source && !running {
                self.start_channel(scene, &spec, recording_path, tape_no);
            } else if (!self.enabled || !*has_source) && running {
                self.stop_channel(scene);
            }
        }
    }

    /// Begin a recording session: channel output redirects into
    /// `<base>/<Scene>` (the recorder passes `<session>/Blackbox`). Call
    /// before stopping the running engines so the next sync restarts them
    /// inside the session.
    pub(crate) fn begin_session(&mut self, base: PathBuf) {
        self.session_base = Some(base);
    }

    /// End the recording session. Returns true when any engine was running
    /// (the recorder stops those so the next tick restarts them back in the
    /// default location, closing the session's last segments cleanly).
    pub(crate) fn end_session(&mut self) -> bool {
        let had_session = self.session_base.take().is_some();
        had_session && self.any_running()
    }

    /// Stop every running engine (used by the restart button and disable).
    pub(crate) fn stop_all(&mut self) {
        for channel in &mut self.channels {
            if let Some(mut engine) = channel.engine.take() {
                engine.stop();
                channel.status = engine.status();
            }
        }
    }

    pub fn any_running(&self) -> bool {
        self.channels
            .iter()
            .any(|c| c.engine.as_ref().is_some_and(|e| e.is_running()))
    }

    /// First channel error, if any (for the settings status chip).
    pub fn first_error(&self) -> Option<String> {
        self.channels
            .iter()
            .find_map(|c| c.status.last_error.clone())
    }

    // -- frames + events ----------------------------------------------------

    /// Forward a captured frame to `scene`'s engine. Non-blocking: the engine
    /// drops the frame (and counts it) if its channel is full or the disk is
    /// critically full. Pass the raw **BGRA** buffer, before the preview path
    /// swaps it to RGBA. No-op when the scene has no running engine.
    pub(crate) fn submit_frame(&self, scene: &str, data: &[u8], width: u32, height: u32) {
        if let Some(engine) = self.channel(scene).and_then(|c| c.engine.as_ref()) {
            engine.submit_frame(data, width, height);
        }
    }

    /// Drain all pending engine events across channels, tagged with their
    /// scene name. Split out from [`Self::apply_events`] so the immutable
    /// borrow of `event_rx` ends before the facade logs.
    pub(crate) fn drain_events(&mut self) -> Vec<(String, BlackboxEvent)> {
        let mut out = Vec::new();
        for channel in &mut self.channels {
            let Some(rx) = channel.event_rx.as_ref() else {
                continue;
            };
            while let Ok(ev) = rx.try_recv() {
                if let RobsEvent::Blackbox(b) = ev {
                    out.push((channel.scene.clone(), b));
                }
            }
        }
        out
    }

    /// Apply drained `(scene, event)` pairs: refresh the per-channel cached
    /// status snapshot on periodic updates and turn the notable discrete
    /// events (start/stop, storage warnings, stalls, recovery, errors) into
    /// complete scene-tagged event-log lines, returned in arrival order for
    /// the facade to log. Per-segment events are logged at info level; with
    /// the default 15-min rotation that is not chatty.
    pub(crate) fn apply_events(
        &mut self,
        events: Vec<(String, BlackboxEvent)>,
    ) -> Vec<(String, EventLogKind)> {
        let mut logs = Vec::new();
        for (scene, ev) in events {
            let channel = match self.channel_mut(&scene) {
                Some(c) => c,
                None => continue,
            };
            match ev {
                BlackboxEvent::StatusUpdated { status } => {
                    channel.status = status;
                }
                BlackboxEvent::Started => {
                    channel.status.running = true;
                    logs.push((
                        format!("Blackbox ({scene}) safety recorder started"),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Stopped => {
                    channel.status.running = false;
                    logs.push((
                        format!("Blackbox ({scene}) safety recorder stopped"),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::SegmentStarted { path, index } => {
                    logs.push((
                        format!("Blackbox ({scene}) segment #{} started: {}", index, path),
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
                            "Blackbox ({scene}) segment #{} closed ({}, {:.1} MiB)",
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
                            "Blackbox ({scene}) disk low: {:.0}% free remaining",
                            free_percent
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::StorageCritical { free_bytes, .. } => {
                    let gib = free_bytes as f64 / 1_073_741_824.0;
                    logs.push((
                        format!(
                            "Blackbox ({scene}) disk critical: pausing ingestion ({:.1} GiB free)",
                            gib
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Stalled { seconds_idle } => {
                    logs.push((
                        format!(
                            "Blackbox ({scene}) stalled: no frames for {}s",
                            seconds_idle
                        ),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Recovered { path } => {
                    logs.push((
                        format!("Blackbox ({scene}) recovered segment: {}", path),
                        EventLogKind::Info,
                    ));
                }
                BlackboxEvent::Error { message } => {
                    channel.status.last_error = Some(message.clone());
                    logs.push((
                        format!("Blackbox ({scene}) error: {}", message),
                        EventLogKind::Info,
                    ));
                }
            }
        }
        logs
    }

    /// Aggregate status line for the settings chip: one summary per channel
    /// joined with "; ". Returns (text, state) with state 0 = neutral,
    /// 1 = good (something recording), 2 = warning (disk paused).
    pub fn status_summary(&self) -> (String, i32) {
        if !self.enabled {
            return ("disabled".to_string(), 0);
        }
        let live: Vec<&BlackboxChannel> = self
            .channels
            .iter()
            .filter(|c| c.engine.as_ref().is_some_and(|e| e.is_running()))
            .collect();
        if live.is_empty() {
            return ("idle (no capture source)".to_string(), 0);
        }
        let mut state = 0;
        let parts: Vec<String> = live
            .iter()
            .map(|c| {
                let status = &c.status;
                if status.disk_paused {
                    state = state.max(2);
                    format!("{}: disk full (paused)", c.scene)
                } else {
                    state = state.max(1);
                    format!("{}: recording", c.scene)
                }
            })
            .collect();
        (parts.join("; "), state)
    }
}

impl RobsController {
    /// Build the per-scene capture situation for the blackbox sync: every
    /// scene with at least one visible capture source becomes a live channel.
    /// (Same rule the ISO scene recorders and the frame taps use: the FIRST
    /// visible capture item of a scene feeds it.)
    fn blackbox_scenes(&self) -> Vec<(String, bool)> {
        let mut scenes: Vec<(String, bool)> = Vec::new();
        for name in self.scenes.scenes().keys().cloned().collect::<Vec<String>>() {
            let has_source = match self.scenes.get(&name) {
                Some(scene) => {
                    let mut found = false;
                    for item in scene.items() {
                        if item.is_visible() && item.capture().is_some() {
                            found = true;
                            break;
                        }
                    }
                    found
                }
                None => false,
            };
            scenes.push((name, has_source));
        }
        scenes.sort_by(|a, b| a.0.cmp(&b.0));
        scenes
    }

    /// Reconcile every channel's run state with the current capture
    /// situation. Delegates to [`BlackboxService::sync`], passing the output
    /// geometry, recording path, and session tape number read off the
    /// controller.
    pub(crate) fn sync_blackbox_engine(&mut self) {
        let spec = OutputSpec {
            width: self.output_width,
            height: self.output_height,
            fps: self.fps_setting,
        };
        let scenes = self.blackbox_scenes();
        let recording_path = self.recording_path.clone();
        let tape_no = self.record.session_tape_no.clone();
        self.blackbox.sync(&scenes, spec, &recording_path, &tape_no);
    }

    pub fn stop_blackbox(&mut self) {
        self.blackbox.stop_all();
    }

    /// Forward a captured frame to `scene`'s engine (see
    /// [`BlackboxService::submit_frame`]).
    pub(crate) fn submit_blackbox_frame(&self, scene: &str, data: &[u8], width: u32, height: u32) {
        self.blackbox.submit_frame(scene, data, width, height);
    }

    /// Drain all pending engine events across channels into a
    /// scene-tagged Vec.
    pub(crate) fn drain_blackbox_events(&mut self) -> Vec<(String, BlackboxEvent)> {
        self.blackbox.drain_events()
    }

    /// Apply drained events: refresh the per-channel status snapshots and
    /// log the notable discrete events, in arrival order.
    pub(crate) fn apply_blackbox_events(&mut self, events: Vec<(String, BlackboxEvent)>) {
        for (message, kind) in self.blackbox.apply_events(events) {
            self.log_event(message, kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_dirs_route_session_settings_and_default() {
        // Session redirect wins and is per-scene.
        let session = PathBuf::from(r"C:\vids\23_09_2026\recording_1\Blackbox");
        assert_eq!(
            BlackboxService::channel_output_dir("", Some(&session), "", "Main"),
            session.join("Main")
        );
        // Settings dir wins over the recording path.
        assert_eq!(
            BlackboxService::channel_output_dir(r"D:\bb", None, r"C:\vids", "Port"),
            PathBuf::from(r"D:\bb").join("Port")
        );
        // Default: <recording path>/Blackbox/<Scene>.
        assert_eq!(
            BlackboxService::channel_output_dir("", None, r"C:\vids", "Main"),
            PathBuf::from(r"C:\vids").join("Blackbox").join("Main")
        );
    }
}
