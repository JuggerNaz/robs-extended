//! Root of the `app` module tree.
//!
//! [`RobsApp`] is the single `eframe` state object. Its definition and a
//! handful of core helpers live here; everything else (device enumeration,
//! capture, recording, settings, annotations, and the UI panels) is split into
//! the sibling submodules under `app/`.
//!
//! Method bodies were moved verbatim from the original monolithic `app.rs`.
//! Methods that are invoked across module boundaries (from `update()` here or
//! from sibling submodules) are marked `pub(crate)`, since Rust only lets a
//! module call another module's *private* items when the caller is a
//! descendant of the defining module. Private helpers that are only used
//! within their own submodule stay private.

mod annotations;
mod anomaly;
mod blackbox;
mod capture;
mod clips;
mod devices;
mod panels;
mod record;
mod settings;
mod state;
mod stream;

use crate::dxgi_capture::DxgiCaptureManager;
use devices::get_audio_devices;
use eframe::egui;
use parking_lot::RwLock;
use robs_chat::aggregator::ChatAggregator;
use robs_chat::message::{ChatEvent, UnifiedChatMessage};
use robs_core::traits::VideoSource;
use robs_core::SceneCollection;
use robs_encoding::detect_encoders;
use robs_profiles::profile::ProfileManager;
use state::{
    AnnotationState, AnomalyState, AudioChannel, AudioDeviceInfo, BlackboxState, EditingState,
    EventLogEntry, EventLogKind, Panel, PreviewState, RecordState, StreamState,
};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct RobsApp {
    streaming: bool,
    streaming_paused: bool,
    event_log: VecDeque<EventLogEntry>,
    show_event_log: bool,
    profile_manager: Arc<RwLock<ProfileManager>>,
    chat_messages: Arc<RwLock<VecDeque<UnifiedChatMessage>>>,
    chat_input: String,
    chat_rx: Option<mpsc::Receiver<ChatEvent>>,
    current_scene: String,
    scenes: SceneCollection, // Migrated to SceneCollection
    show_settings: bool,
    #[allow(dead_code)]
    settings_rect: Option<egui::Rect>,
    active_settings_tab: usize,
    audio_channels: Vec<AudioChannel>,
    streaming_time: u64,
    bitrate: u32,
    dropped_frames: u64,
    fps: f64,
    #[allow(dead_code)]
    active_panel: Panel,
    show_preview: bool,
    show_scenes: bool,
    show_controls: bool,
    show_audio: bool,
    show_chat: bool,
    show_stats: bool,
    confirm_on_exit: bool,
    minimize_to_tray: bool,
    always_on_top: bool,
    check_for_updates: bool,
    filename_formatting: String,
    base_width: u32,
    base_height: u32,
    output_width: u32,
    output_height: u32,
    fps_setting: f32,
    stream_service: String,
    stream_server: String,
    stream_key: String,
    stream_bitrate: u32,
    keyframe_interval: u32,
    recording_bitrate: u32,
    recording_path: String,
    recording_format: String,
    video_encoder: String,
    audio_encoder: String,
    audio_sample_rate: String,  // "44100" or "48000"
    audio_channel_mode: String, // "mono" or "stereo"
    available_video_encoders: Vec<String>,
    available_audio_encoders: Vec<String>,
    // Audio devices
    audio_devices: Vec<AudioDeviceInfo>,
    #[allow(dead_code)]
    selected_audio_device: String,
    nvenc_available: bool,
    aac_available: bool,
    ffmpeg_available: bool,
    // Active video source (kept flat: a single transient box).
    #[allow(dead_code)]
    active_video_source: Option<Box<dyn VideoSource>>,
    // Direct DXGI Desktop Duplication capture (GPU-accelerated), shared by
    // preview and recording.
    dxgi_manager: Option<DxgiCaptureManager>,
    // Cohesive state clusters (definitions in `state.rs`).
    record: RecordState,
    stream: StreamState,
    preview: PreviewState,
    annotation: AnnotationState,
    editing: EditingState,
    // Maps scene-item IDs to window handles (HWND) for window-capture sources.
    window_hwnds: std::collections::HashMap<robs_core::SceneItemId, isize>,
    // Active webcam capture sessions keyed by scene-item ID.
    webcam_captures: std::collections::HashMap<
        robs_core::SceneItemId,
        robs_sources::native_capture::WebcamCapture,
    >,
    // Flag to capture a snapshot on the next recorded frame.
    take_snapshot: bool,
    // Monotonic counter so rapid snapshots don't collide within the same second.
    snapshot_seq: u64,
    // When set, a "Snapshot saved" toast is rendered; cleared once it fades.
    snapshot_flash: Option<std::time::Instant>,
    // Text overlays (persistent on-screen text baked into recordings).
    text_overlays: Vec<robs_core::TextOverlay>,
    overlay_text_input: String,
    // Always-on background safety recorder.
    blackbox: BlackboxState,
    // User-toggled short-clip anomaly capture buffer.
    anomaly: AnomalyState,
}

impl RobsApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
        });

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
            show_event_log: true,
            profile_manager: Arc::new(RwLock::new(ProfileManager::default())),
            chat_messages: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            chat_input: String::new(),
            chat_rx: None,
            current_scene: "Main Scene".to_string(),
            scenes: {
                let mut col = SceneCollection::new();
                col.create_scene("Main Scene".to_string());
                col.set_current_scene("Main Scene");
                col
            },
            show_settings: false,
            settings_rect: None,
            audio_channels: vec![
                AudioChannel {
                    name: "Mic/Aux".into(),
                    volume: 0.8,
                    muted: false,
                    device_id: "default".to_string(),
                    is_desktop: false,
                },
                AudioChannel {
                    name: "Desktop Audio".into(),
                    volume: 0.6,
                    muted: false,
                    device_id: "default".to_string(),
                    is_desktop: true,
                },
            ],
            streaming_time: 0,
            bitrate: 6000,
            dropped_frames: 0,
            fps: 30.0,
            active_panel: Panel::Preview,
            show_preview: true,
            show_scenes: true,
            show_controls: true,
            show_audio: true,
            show_chat: true,
            show_stats: true,
            active_settings_tab: 0,
            confirm_on_exit: true,
            minimize_to_tray: false,
            always_on_top: false,
            check_for_updates: true,
            filename_formatting: "%CCYY-%MM-%DD %hh-%mm-%ss".into(),
            base_width: 1920,
            base_height: 1080,
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
            audio_sample_rate: "48000".to_string(),
            audio_channel_mode: "stereo".to_string(),
            available_video_encoders: video_encoders,
            available_audio_encoders: audio_encoders,
            // Audio devices
            audio_devices: get_audio_devices(),
            selected_audio_device: "0".to_string(),
            nvenc_available: detection.nvenc_available,
            aac_available: detection.aac_available,
            ffmpeg_available: detection.ffmpeg_available,
            active_video_source: None,
            // Direct DXGI Desktop Duplication capture
            dxgi_manager: None, // Initialized lazily on first capture
            // Cohesive state clusters (see `state.rs`)
            record: RecordState {
                recording: false,
                recording_paused: false,
                recording_time: 0,
                recording_start_time: None,
                last_recording_path: String::new(),
                recording_file_output: None,
                ffmpeg_recording_handle: None,
                recording_dxgi_thread: None,
                recording_stop_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                recording_frame_sender: None,
                recording_ffmpeg_stdin: None,
                last_frame_time: None,
                timer_last_tick: None,
                frame_count: 0,
                clip_marks: Vec::new(),
                clip_mark_start: None,
                clip_export_rx: None,
                clip_export_pending: 0,
                clip_marking_supported: false,
            },
            stream: StreamState {
                ffmpeg_handle: None,
                writer_thread: None,
                stop_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                frame_sender: None,
                timer_last_tick: None,
                frame_count: 0,
            },
            preview: PreviewState {
                preview_capture_active: false,
                preview_frame_sender: None,
                preview_frame_receiver: None,
                preview_capture_handle: None,
                preview_frame_count: 0,
                last_preview_capture: std::time::Instant::now(),
                frame_buffer: std::collections::HashMap::new(),
                preview_textures: std::collections::HashMap::new(),
                last_output_frame: None,
            },
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
            overlay_text_input: String::new(),
            blackbox: {
                let (bus, rx) = robs_core::EventBus::new();
                let mut settings = robs_profiles::settings::BlackboxSettings::default();
                // Default the encoder to the best available hardware/software.
                if detection.nvenc_available {
                    settings.encoder = "h264_nvenc".into();
                } else {
                    settings.encoder = "libx264".into();
                }
                BlackboxState {
                    enabled: settings.enabled,
                    settings,
                    engine: None,
                    status: robs_core::event::BlackboxStatus::default(),
                    event_tx: bus.tx(),
                    event_rx: Some(rx),
                }
            },
            anomaly: {
                let (bus, rx) = robs_core::EventBus::new();
                AnomalyState {
                    enabled: false,
                    // Persisted settings (config dir `settings.json`); defaults
                    // on first run or an unreadable file.
                    settings: robs_profiles::settings::AnomalySettings::load_or_default(),
                    engine: None,
                    status: robs_core::event::AnomalyStatus::default(),
                    event_tx: bus.tx(),
                    event_rx: Some(rx),
                }
            },
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

    fn handle_events(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &mut self.chat_rx {
            while let Ok(event) = rx.try_recv() {
                if let ChatEvent::Message(msg) = event {
                    self.chat_messages.write().push_back(*msg);
                }
            }
        }
        // Elapsed timers advance by the wall-clock delta between UI ticks,
        // NOT by one per tick: egui repaints on every input event, so a
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
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
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
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
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
            // Status updates arrive ~1/s; keep the UI repainting so the chip/bar stay live.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Drain Anomaly engine events: refresh the status snapshot and log
        // clip lifecycle events.
        let anomaly_events = self.drain_anomaly_events();
        if !anomaly_events.is_empty() {
            self.apply_anomaly_events(anomaly_events);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Drain clip-export results from the post-stop cutting thread: log
        // each saved/failed clip and update the busy counter.
        let clip_events = self.drain_clip_export_events();
        if !clip_events.is_empty() {
            self.apply_clip_export_events(clip_events);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn format_time(seconds: u64) -> String {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        let s = seconds % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }

    fn log_event(&mut self, message: impl Into<String>, kind: EventLogKind) {
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

impl eframe::App for RobsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events(ctx);

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
        self.process_preview_frames(ctx);

        // Encoders timestamp piped frames at a FIXED framerate, and the preview
        // must stay live while a capture source exists. egui is otherwise fully
        // event-driven: with no repaint request, update() only runs on input
        // events, so the preview freezes on its placeholder and the blackbox
        // tap starves until the next mouse move. Tick at the frame interval
        // whenever a visible capture source exists (encoder pacing keeps the
        // video from fast-forwarding through idle stretches on playback).
        if has_capture_source {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(
                1.0 / self.fps_setting.max(1.0),
            ));
        }

        // Manage preview capture based on source visibility.
        if has_capture_source && !self.preview.preview_capture_active {
            self.start_preview_capture();
        } else if !has_capture_source && self.preview.preview_capture_active {
            self.stop_preview_capture();
        }

        self.menu_bar(ctx);
        self.annotation_toolbar(ctx);
        self.streaming_controls(ctx);
        self.scenes_panel(ctx);
        self.source_properties_modal(ctx);
        self.right_panel(ctx);
        self.preview_panel(ctx);

        if self.show_settings {
            self.show_settings_window(ctx);
        }
    }
}

/// User home directory, portable across platforms
/// (`%USERPROFILE%` on Windows, `$HOME` elsewhere). `None` when neither is set.
pub(crate) fn user_home() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
}

/// Default base folder for recordings, snapshots, and engine output:
/// `<home>/Videos` on Windows/Linux, `<home>/Movies` on macOS.
pub(crate) fn default_videos_dir() -> String {
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
