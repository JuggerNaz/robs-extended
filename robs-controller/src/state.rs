//! Support types extracted out of the former `app.rs` monolith.
//!
//! These describe devices, audio channels, and the event-log entry types, plus
//! the cohesive engine state clusters. They are `pub`: they cross the crate
//! boundary now, with the view layer (`robs-ui`) reading them through
//! `RobsController`.

#[derive(Clone)]
pub struct MonitorInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub position_x: i32,
    pub position_y: i32,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub id: String,
    pub is_input: bool, // true = microphone/aux, false = desktop audio/speakers
}

#[derive(Clone)]
pub struct AudioChannel {
    pub name: String,
    pub volume: f32,
    pub muted: bool,
    pub device_id: String, // device ID or "disabled" or "default"
    pub is_desktop: bool,  // true = desktop audio, false = mic/aux
}

pub struct EventLogEntry {
    pub timestamp: chrono::DateTime<chrono::Local>,
    pub message: String,
    pub kind: EventLogKind,
}

#[derive(Clone, Copy, PartialEq)]
pub enum EventLogKind {
    Stream,
    Record,
    Info,
    Annotation,
    Overlay,
}

/// Recording-session runtime state.
///
/// `last_frame_time` and `frame_count` replace the former `static mut`
/// `LAST_RECORDING_FRAME` / `FRAME_COUNT` in `send_frame_to_recording`, which
/// were read and written without synchronization and therefore undefined
/// behavior. They are now ordinary fields reached only through `&mut self` on
/// the UI thread (the spawned writer thread never touches them — it only drains
/// the frame channel into FFmpeg's stdin).
#[allow(dead_code)]
pub struct RecordState {
    pub recording: bool,
    pub recording_paused: bool,
    /// Elapsed recording time in **milliseconds**, accumulated from the UI
    /// tick's wall-clock delta. (It used to be `+= 1` per repaint, which
    /// raced whenever mouse input raised the repaint rate.)
    pub recording_time: u64,
    pub recording_start_time: Option<u64>,
    pub last_recording_path: String,
    pub recording_file_output: Option<robs_outputs::FileOutput>,
    pub ffmpeg_recording_handle: Option<std::process::Child>,
    pub recording_dxgi_thread: Option<std::thread::JoinHandle<()>>,
    pub recording_stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub recording_frame_sender: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    pub recording_ffmpeg_stdin: Option<std::process::ChildStdin>,
    pub last_frame_time: Option<std::time::Instant>,
    /// Wall-clock anchor for the elapsed-time accumulator. `None` while
    /// paused or stopped so paused time is excluded on resume.
    pub timer_last_tick: Option<std::time::Instant>,
    pub frame_count: u64,
    /// Closed clip marks (frame positions) for the current recording session.
    /// See `clips.rs`: file position = frame_count / fps, so a mark is just
    /// two frame-position integers; clips are stream-copied after stop.
    pub clip_marks: Vec<super::clips::ClipMark>,
    /// Start frame of the currently open clip mark, if any (Mark In pressed,
    /// Mark Out not yet).
    pub clip_mark_start: Option<u64>,
    /// Results from the post-stop clip export thread, drained each tick in
    /// `handle_events` (same pattern as the Anomaly engine channel).
    pub clip_export_rx: Option<std::sync::mpsc::Receiver<super::clips::ClipExportResult>>,
    /// Clip exports still in flight (greys the Mark button while > 0).
    pub clip_export_pending: u32,
    /// True when this session pipes frames from the UI tick (DXGI / webcam
    /// rawvideo pipeline) — the only pipeline where `frame_count` anchors
    /// file position. The gdigrab window-capture path lets FFmpeg pull frames
    /// itself, so marking there stays disabled.
    pub clip_marking_supported: bool,
}

/// RTMP streaming-session runtime state.
///
/// Mirrors the process machinery of `RecordState`: FFmpeg is spawned by
/// `start_streaming()` (`stream.rs`), frames produced by the UI tick are
/// drained by a dedicated writer thread into FFmpeg's stdin, and shutdown
/// follows the same stop-flag → drop-sender → join-thread → wait-child order.
/// The flat `streaming` / `streaming_paused` / `streaming_time` fields on
/// `RobsApp` remain the UI-level view of this state.
#[allow(dead_code)]
pub struct StreamState {
    pub ffmpeg_handle: Option<std::process::Child>,
    pub writer_thread: Option<std::thread::JoinHandle<()>>,
    pub stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub frame_sender: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    /// Wall-clock anchor for `streaming_time` (milliseconds), mirroring
    /// `RecordState::timer_last_tick`.
    pub timer_last_tick: Option<std::time::Instant>,
    pub frame_count: u64,
}

/// One captured preview frame for a scene item: raw RGBA pixels plus the
/// dimensions and the `next_frame_version` value at capture time. View layers
/// turn these into GPU textures/images themselves and use `version` to skip
/// re-uploading a frame they have already consumed.
pub struct PreviewFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub version: u64,
}

/// Live preview-capture state: per-source frame buffers, raw preview frames
/// for the view layer, and the capture-rate throttle.
#[allow(dead_code)]
pub struct PreviewState {
    pub preview_capture_active: bool,
    pub preview_frame_sender: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    pub preview_frame_receiver: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    pub preview_capture_handle: Option<std::process::Child>,
    pub preview_frame_count: u64,
    pub last_preview_capture: std::time::Instant,
    pub frame_buffer: std::collections::HashMap<String, Vec<u8>>,
    /// The latest RGBA preview frame per scene item (replaces the former
    /// UI-toolkit texture map; the view uploads these itself).
    pub preview_frames: std::collections::HashMap<robs_core::SceneItemId, PreviewFrame>,
    /// Monotonic counter bumped every time a frame is captured; stored on each
    /// `PreviewFrame` so views can detect fresh frames.
    pub next_frame_version: u64,
    /// The last composed output-resolution BGRA frame handed to the encoders.
    /// Re-sent when a UI tick produces no fresh frame (static screen, DXGI
    /// timeout, webcam lag) so FFmpeg's constant-framerate input stays paced
    /// to wall clock — the fix for fast-forwarding recordings.
    pub last_output_frame: Option<Vec<u8>>,
}

/// Annotation / markup tool state.
pub struct AnnotationState {
    pub show_annotations: bool,
    pub annotations: Vec<robs_core::Annotation>,
    pub annotation_tool: robs_core::AnnotationTool,
    pub annotation_style: robs_core::AnnotationStyle,
    pub annotation_drawing: Option<robs_core::Annotation>,
    pub selected_annotation: Option<robs_core::AnnotationId>,
    pub editing_text_id: Option<robs_core::AnnotationId>,
    pub text_input: String,
    pub record_font: Option<ab_glyph::FontVec>,
}

/// Always-on Blackbox Dual Recording Engine state.
///
/// Owns the engine (lazily (re)built from `settings`), the latest published
/// status snapshot, and the event channel the engine reports through. The UI
/// drains `event_rx` each frame to refresh `status` and log notable events.
pub struct BlackboxState {
    /// Master on/off. Honored live: toggling off stops a running engine.
    pub enabled: bool,
    /// Editable settings; projected into a fresh `BlackboxConfig` whenever the
    /// engine (re)starts, so most edits take effect on the next start.
    pub settings: robs_profiles::settings::BlackboxSettings,
    /// The engine, if it has ever been started this session.
    pub engine: Option<robs_outputs::BlackboxEngine>,
    /// Latest health snapshot (refreshed from events).
    pub status: robs_core::event::BlackboxStatus,
    /// Event channel the engine publishes on.
    pub event_tx: robs_core::event::EventTx,
    pub event_rx: Option<robs_core::event::EventRx>,
}

/// Short Clip Anomaly Capture engine state.
///
/// User-toggled rolling buffer. Unlike Blackbox, the engine is started/stopped
/// explicitly via the UI (Start/Stop Buffer), not auto-synced to capture state.
pub struct AnomalyState {
    /// Master on/off (toggled live by the Start/Stop button).
    pub enabled: bool,
    pub settings: robs_profiles::settings::AnomalySettings,
    pub engine: Option<robs_outputs::AnomalyCaptureEngine>,
    pub status: robs_core::event::AnomalyStatus,
    pub event_tx: robs_core::event::EventTx,
    pub event_rx: Option<robs_core::event::EventRx>,
}

/// Source-properties modal editing state.
pub struct EditingState {
    pub show_source_properties: bool,
    pub editing_source_id: Option<robs_core::SceneItemId>,
    pub editing_source_name: String,
    pub editing_source_pos_x: f32,
    pub editing_source_pos_y: f32,
    pub editing_source_scale_x: f32,
    pub editing_source_scale_y: f32,
    pub editing_source_rotation: f32,
    pub editing_source_crop_left: u32,
    pub editing_source_crop_top: u32,
    pub editing_source_crop_right: u32,
    pub editing_source_crop_bottom: u32,
}
