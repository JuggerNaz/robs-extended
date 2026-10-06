//! Framework-agnostic engine/orchestration core for ROBS.
//!
//! [`RobsController`] owns all engine state (scenes, capture, recording,
//! streaming, blackbox, anomaly, annotations) and drives the engine pipelines
//! once per [`RobsController::tick`]. It contains no UI-toolkit types: the
//! former `App::update` body minus rendering became `tick()`, context
//! parameters are gone (their only job was scheduling repaints — the view now
//! owns that via the wake hint `tick()` returns), and preview textures became
//! raw RGBA frames with a version counter the view uploads itself.
//!
//! Method bodies were moved verbatim from the former `robs-ui` monolith.
//! Methods that the view layer calls (start/stop recording or streaming,
//! clip marking, anomaly buffers, logging, …) are `pub`; helpers only used
//! within this crate stay `pub(crate)` or private.

mod anomaly;
mod blackbox;
mod capture;
mod clips;
pub mod db;
mod overlay;
pub mod qid;
mod record;
mod stream;
pub mod telemetry;

pub mod annotation_raster;
pub mod devices;
pub mod dxgi_capture;
pub mod state;

use capture::PreviewService;
use parking_lot::RwLock;
use robs_chat::aggregator::ChatAggregator;
use robs_chat::message::{ChatEvent, UnifiedChatMessage};
use robs_core::traits::VideoSource;
use robs_core::SceneCollection;
use robs_encoding::detect_encoders;
use anomaly::AnomalyService;
use blackbox::BlackboxService;
use overlay::OverlayService;
use record::RecordService;
use state::{AnnotationState, EditingState, EventLogEntry, EventLogKind};
use stream::StreamService;
use telemetry::TelemetryService;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct RobsController {
    pub streaming: bool,
    pub streaming_paused: bool,
    pub event_log: VecDeque<EventLogEntry>,
    pub chat_messages: Arc<RwLock<VecDeque<UnifiedChatMessage>>>,
    chat_rx: Option<mpsc::Receiver<ChatEvent>>,
    pub scenes: SceneCollection, // Migrated to SceneCollection
    pub streaming_time: u64,
    pub bitrate: u32,
    pub dropped_frames: u64,
    pub fps: f64,
    pub output_width: u32,
    pub output_height: u32,
    pub fps_setting: f32,
    pub stream_service: String,
    pub stream_server: String,
    pub stream_key: String,
    pub stream_bitrate: u32,
    pub keyframe_interval: u32,
    pub recording_bitrate: u32,
    pub recording_path: String,
    pub recording_format: String,
    pub video_encoder: String,
    pub audio_encoder: String,
    pub available_video_encoders: Vec<String>,
    pub available_audio_encoders: Vec<String>,
    pub nvenc_available: bool,
    pub aac_available: bool,
    pub ffmpeg_available: bool,
    // Active video source (kept flat: a single transient box).
    #[allow(dead_code)]
    pub active_video_source: Option<Box<dyn VideoSource>>,
    // Cohesive state clusters (definitions in `state.rs`). Services own
    // their cluster; field access flows through `Deref`, so view code is
    // unchanged. The preview service also owns the shared DXGI capture
    // manager; the window-HWND and webcam-capture maps stay flat on the
    // facade because the view layer mutates them in place.
    pub record: RecordService,
    pub stream: StreamService,
    pub preview: PreviewService,
    pub annotation: AnnotationState,
    pub editing: EditingState,
    // Maps scene-item IDs to window handles (HWND) for window-capture sources.
    pub window_hwnds: std::collections::HashMap<robs_core::SceneItemId, isize>,
    // Active webcam capture sessions keyed by scene-item ID.
    pub webcam_captures: std::collections::HashMap<
        robs_core::SceneItemId,
        robs_sources::native_capture::WebcamCapture,
    >,
    // Flag to capture a snapshot on the next recorded frame.
    pub take_snapshot: bool,
    // Monotonic counter so rapid snapshots don't collide within the same second.
    pub snapshot_seq: u64,
    // When set, a "Snapshot saved" toast is rendered; cleared once it fades.
    pub snapshot_flash: Option<std::time::Instant>,
    // Text overlays (persistent on-screen text baked into recordings).
    pub text_overlays: Vec<robs_core::TextOverlay>,
    // Always-on background safety recorder. Service owning the
    // `BlackboxState` cluster (field access flows through `Deref`).
    pub blackbox: BlackboxService,
    // User-toggled short-clip anomaly capture buffer. Service owning the
    // `AnomalyState` cluster (field access flows through `Deref`).
    pub anomaly: AnomalyService,
    // Serial data-string telemetry feed (ROV nav strings over a COM port).
    // Service owning the `TelemetryState` cluster (field access flows
    // through `Deref`, so view code is unchanged).
    pub telemetry: TelemetryService,
    // Scene overlays: the baked data-string boxes + the company logo.
    // Service owning the `OverlayState` cluster (field access flows through
    // `Deref`, so view code is unchanged).
    pub overlay: OverlayService,
    // QID rail: structure components from the inspection DB + click-marked
    // recording time segments (see `qid.rs` / `db.rs`). Service owning the
    // `QidState` cluster (field access flows through `Deref`).
    pub qid: qid::QidService,
}

impl RobsController {
    pub fn new() -> Self {
        let mut this = Self::build();
        // Restore the scene-overlay settings (toggles, logo file, placement).
        let ov = robs_profiles::settings::OverlaySettings::load_or_default();
        this.overlay.data_string_enabled = ov.data_string_enabled;
        this.overlay.logo_position = robs_core::Position::new(ov.logo_x, ov.logo_y);
        this.overlay.logo_height_fraction = ov.logo_height_fraction.clamp(0.02, 0.5);
        if !ov.logo_path.is_empty() {
            this.set_logo_from_path(ov.logo_path);
        }
        // The logo toggle is only meaningful with a decodable file behind it.
        this.overlay.logo_enabled = ov.logo_enabled && this.overlay.logo.is_some();
        // Auto-connect the telemetry feed on launch when enabled (settings
        // are persisted; the reader retries while the port is unavailable).
        if this.telemetry.settings.enabled {
            this.start_telemetry();
        }
        // Auto-load the QID list when the inspection DB is configured.
        this.init();
        this
    }

    fn build() -> Self {
        let detection = detect_encoders();

        let mut video_encoders = Vec::new();
        if detection.ffmpeg_available {
            video_encoders.push("FFmpeg x264 (Software)".into());
        }
        if detection.nvenc_available {
            video_encoders.push("NVIDIA NVENC H.264 (Hardware)".into());
        }
        if video_encoders.is_empty() {
            video_encoders.push("None Available".into());
        }

        let mut audio_encoders = Vec::new();
        if detection.aac_available {
            audio_encoders.push("FFmpeg AAC".into());
        }
        if audio_encoders.is_empty() {
            audio_encoders.push("None Available".into());
        }

        let video_encoder = if detection.nvenc_available {
            "NVIDIA NVENC H.264 (Hardware)".into()
        } else if detection.ffmpeg_available {
            "FFmpeg x264 (Software)".into()
        } else {
            "None Available".into()
        };

        let audio_encoder = if detection.aac_available {
            "FFmpeg AAC".into()
        } else {
            "None Available".into()
        };

        Self {
            streaming: false,
            streaming_paused: false,
            event_log: VecDeque::new(),
            chat_messages: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            chat_rx: None,
            scenes: {
                let mut col = SceneCollection::new();
                col.create_scene("Main".to_string());
                col.set_current_scene("Main");
                col
            },
            streaming_time: 0,
            bitrate: 6000,
            dropped_frames: 0,
            fps: 30.0,
            output_width: 1280,
            output_height: 720,
            fps_setting: 30.0,
            stream_service: "YouTube".into(),
            stream_server: "rtmp://a.rtmp.youtube.com/live2".into(),
            stream_key: String::new(),
            stream_bitrate: 6000,
            keyframe_interval: 2,
            recording_bitrate: 10000,
            recording_path: String::new(),
            recording_format: "mp4".into(),
            video_encoder,
            audio_encoder,
            available_video_encoders: video_encoders,
            available_audio_encoders: audio_encoders,
            nvenc_available: detection.nvenc_available,
            aac_available: detection.aac_available,
            ffmpeg_available: detection.ffmpeg_available,
            active_video_source: None,
            // Cohesive state clusters (see `state.rs`)
            record: RecordService::new(),
            stream: StreamService::new(),
            preview: PreviewService::new(),
            annotation: AnnotationState {
                show_annotations: true,
                annotations: Vec::new(),
                annotation_tool: robs_core::AnnotationTool::default(),
                annotation_style: robs_core::AnnotationStyle::default(),
                annotation_drawing: None,
                selected_annotation: None,
                editing_text_id: None,
                text_input: String::new(),
                record_font: None,
            },
            editing: EditingState {
                show_source_properties: false,
                editing_source_id: None,
                editing_source_name: String::new(),
                editing_source_pos_x: 0.0,
                editing_source_pos_y: 0.0,
                editing_source_scale_x: 1.0,
                editing_source_scale_y: 1.0,
                editing_source_rotation: 0.0,
                editing_source_crop_left: 0,
                editing_source_crop_top: 0,
                editing_source_crop_right: 0,
                editing_source_crop_bottom: 0,
            },
            window_hwnds: std::collections::HashMap::new(),
            webcam_captures: std::collections::HashMap::new(),
            take_snapshot: false,
            snapshot_seq: 0,
            snapshot_flash: None,
            text_overlays: Vec::new(),
            blackbox: BlackboxService::new(detection.nvenc_available),
            anomaly: AnomalyService::new(),
            telemetry: TelemetryService::new(
                robs_profiles::settings::SerialTelemetrySettings::load_or_default(),
            ),
            overlay: OverlayService::default(),
            qid: {
                let settings = robs_profiles::settings::DatabaseSettings::load_or_default();
                let worker = settings
                    .is_configured()
                    .then(|| db::spawn(settings.clone()));
                let mut service = qid::QidService::new(settings);
                service.db = worker;
                service
            },
        }
    }

    /// Load the QID list at launch when the database is configured (the
    /// telemetry analogue of auto-connect).
    pub fn init(&mut self) {
        if self.qid.settings.is_configured() && self.qid.db.is_some() {
            self.refresh_qids();
        }
    }

    pub fn with_chat(
        mut self,
        _aggregator: Arc<ChatAggregator>,
        rx: mpsc::Receiver<ChatEvent>,
    ) -> Self {
        self.chat_rx = Some(rx);
        self
    }

    /// Drive one engine tick: the former `App::update` body minus all
    /// rendering. Returns the "next wake" hint — the soonest of every repaint
    /// deadline the old code issued this tick — for the view to schedule its
    /// next repaint (`None` when no engine timer needs one).
    pub fn tick(&mut self) -> Option<std::time::Duration> {
        let mut wake = self.handle_events();

        // Determine whether the current scene has a visible capture source.
        // Computed up-front so both the preview-capture lifecycle and the
        // Blackbox engine can react within the same tick, before frames are
        // processed/tapped.
        let has_capture_source = self
            .scenes
            .current_scene()
            .map(|s| {
                s.items()
                    .iter()
                    .any(|i| i.is_visible() && i.capture().is_some())
            })
            .unwrap_or(false);

        // Start/stop the always-on Blackbox engine based on capture state.
        // Done before process_preview_frames so the engine is guaranteed to
        // be running when the per-frame tap fires.
        self.sync_blackbox_engine(has_capture_source);

        // Process preview frames (recording + blackbox tap hook into this).
        self.process_preview_frames();

        // Encoders timestamp piped frames at a FIXED framerate, and the preview
        // must stay live while a capture source exists. The UI is otherwise
        // fully event-driven: with no repaint request it only runs on input
        // events, so the preview freezes on its placeholder and the blackbox
        // tap starves until the next mouse move. Wake at the frame interval
        // whenever a visible capture source exists (encoder pacing keeps the
        // video from fast-forwarding through idle stretches on playback).
        if has_capture_source {
            wake = min_wake(
                wake,
                std::time::Duration::from_secs_f32(1.0 / self.fps_setting.max(1.0)),
            );
        }

        // Manage preview capture based on source visibility.
        if has_capture_source && !self.preview.preview_capture_active {
            self.preview.start_capture();
        } else if !has_capture_source && self.preview.preview_capture_active {
            self.preview.stop_capture();
        }

        wake
    }

    /// Drain chat + engine event channels, advance the elapsed timers, and
    /// reconcile engine liveness. Returns the earliest wake deadline wanted
    /// this tick (formerly issued as repaint requests on the UI context; the
    /// view applies it now).
    fn handle_events(&mut self) -> Option<std::time::Duration> {
        let mut wake: Option<std::time::Duration> = None;
        if let Some(rx) = &mut self.chat_rx {
            while let Ok(event) = rx.try_recv() {
                if let ChatEvent::Message(msg) = event {
                    self.chat_messages.write().push_back(*msg);
                }
            }
        }
        // Elapsed timers advance by the wall-clock delta between UI ticks,
        // NOT by one per tick: the UI repaints on every input event, so a
        // per-tick increment raced the timer whenever the mouse moved.
        if self.streaming {
            if !self.streaming_paused {
                let now = std::time::Instant::now();
                if let Some(last) = self.stream.timer_last_tick {
                    self.streaming_time += last.elapsed().as_millis() as u64;
                }
                self.stream.timer_last_tick = Some(now);
            } else {
                self.stream.timer_last_tick = None;
            }
            wake = min_wake(wake, std::time::Duration::from_secs(1));
        }
        if self.record.recording {
            if !self.record.recording_paused {
                let now = std::time::Instant::now();
                if let Some(last) = self.record.timer_last_tick {
                    self.record.recording_time += last.elapsed().as_millis() as u64;
                }
                self.record.timer_last_tick = Some(now);
            } else {
                self.record.timer_last_tick = None;
            }
            wake = min_wake(wake, std::time::Duration::from_secs(1));
        }

        // If the streaming FFmpeg died on its own (bad key, network drop,
        // ingest rejection), surface it instead of ticking a ghost LIVE timer.
        if self.streaming {
            let ffmpeg_alive = self
                .stream
                .ffmpeg_handle
                .as_mut()
                .map(|child| child.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            if !ffmpeg_alive {
                self.stop_streaming();
                self.log_event("Stream ended unexpectedly", EventLogKind::Stream);
            }
        }

        // Auto-clear the snapshot toast once it has faded.
        if let Some(t) = self.snapshot_flash {
            if t.elapsed() > std::time::Duration::from_millis(2000) {
                self.snapshot_flash = None;
            }
        }

        // Drain Blackbox engine events: refresh the status snapshot and log
        // notable events. Collect first, then mutate, to avoid holding an
        // immutable borrow of `event_rx` across the `&mut self` log calls.
        let blackbox_events = self.drain_blackbox_events();
        if !blackbox_events.is_empty() {
            self.apply_blackbox_events(blackbox_events);
            // Status updates arrive ~1/s; keep the view repainting so the
            // chip/bar stay live.
            wake = min_wake(wake, std::time::Duration::from_millis(500));
        }

        // Drain Anomaly engine events: refresh the status snapshot and log
        // clip lifecycle events.
        let anomaly_events = self.drain_anomaly_events();
        if !anomaly_events.is_empty() {
            self.apply_anomaly_events(anomaly_events);
            wake = min_wake(wake, std::time::Duration::from_millis(500));
        }

        // Drain clip-export results from the post-stop cutting thread: log
        // each saved/failed clip and update the busy counter.
        let clip_events = self.drain_clip_export_events();
        if !clip_events.is_empty() {
            self.apply_clip_export_events(clip_events);
            wake = min_wake(wake, std::time::Duration::from_millis(100));
        }

        // While exports are still cutting in the background, keep ticking so
        // their results surface promptly (replaces the export thread's old
        // repaint request; cadence matches the 100 ms above).
        if self.record.clip_export_pending > 0 {
            wake = min_wake(wake, std::time::Duration::from_millis(100));
        }

        // Drain QID DB worker results (component lists, insert outcomes);
        // keep ticking while a load/save is in flight.
        if self.drain_qid_db_events() {
            wake = min_wake(wake, std::time::Duration::from_millis(100));
        }

        wake
    }

    pub fn format_time(seconds: u64) -> String {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        let s = seconds % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }

    pub fn log_event(&mut self, message: impl Into<String>, kind: EventLogKind) {
        let entry = EventLogEntry {
            timestamp: chrono::Local::now(),
            message: message.into(),
            kind,
        };
        self.event_log.push_back(entry);
        if self.event_log.len() > 200 {
            self.event_log.pop_front();
        }
    }

    fn save_snapshot(&mut self, rgba: &[u8], width: u32, height: u32) {
        // Link the snapshot to the active recording session: save into a
        // `Snapshots` subfolder next to the recording file, named after it.
        // Fall back to %USERPROFILE%\Videos\Snapshots\ if there is no recording.
        let (dir, stem) = if self.record.recording && !self.record.last_recording_path.is_empty() {
            let rec_path = std::path::Path::new(&self.record.last_recording_path);
            let rec_stem = rec_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ROBS_Recording".to_string());
            let rec_dir = rec_path
                .parent()
                .map(|p| p.join("Snapshots"))
                .unwrap_or_else(|| std::path::PathBuf::from("Snapshots"));
            (rec_dir.to_string_lossy().into_owned(), rec_stem)
        } else {
            let fallback = format!("{}/Snapshots", default_videos_dir());
            (fallback, "ROBS_Snapshot".to_string())
        };

        let _ = std::fs::create_dir_all(&dir);
        let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let seq = self.snapshot_seq;
        self.snapshot_seq = self.snapshot_seq.wrapping_add(1);
        let path = std::path::Path::new(&dir)
            .join(format!("{}_snapshot_{}_{}.png", stem, ts, seq))
            .to_string_lossy()
            .into_owned();

        if let Some(img) = image::RgbaImage::from_raw(width, height, rgba.to_vec()) {
            if img.save(&path).is_ok() {
                self.snapshot_flash = Some(std::time::Instant::now());
                self.log_event(format!("Snapshot saved: {}", path), EventLogKind::Info);
            }
        }
    }
}

/// Earliest-wake reduction: the former per-tick repaint requests accumulated
/// as the minimum deadline, so combining hints keeps that exact semantics
/// (`None` = nothing requested a wake yet).
fn min_wake(
    wake: Option<std::time::Duration>,
    candidate: std::time::Duration,
) -> Option<std::time::Duration> {
    Some(wake.map_or(candidate, |w| w.min(candidate)))
}

/// User home directory, portable across platforms
/// (`%USERPROFILE%` on Windows, `$HOME` elsewhere). `None` when neither is set.
pub fn user_home() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
}

/// Default base folder for recordings, snapshots, and engine output:
/// `<home>/Videos` on Windows/Linux, `<home>/Movies` on macOS.
pub fn default_videos_dir() -> String {
    match user_home() {
        Some(home) => {
            #[cfg(target_os = "macos")]
            let folder = "Movies";
            #[cfg(not(target_os = "macos"))]
            let folder = "Videos";
            std::path::Path::new(&home)
                .join(folder)
                .to_string_lossy()
                .into_owned()
        }
        None => "Videos".to_string(),
    }
}
