use crate::types::*;
use serde::{Deserialize, Serialize};
use flume::{Sender, Receiver, unbounded};

pub type EventTx = Sender<RobsEvent>;
pub type EventRx = Receiver<RobsEvent>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RobsEvent {
    Session(SessionEvent),
    Source(SourceEvent),
    Encoder(EncoderEvent),
    Output(OutputEvent),
    Scene(SceneEvent),
    Profile(ProfileEvent),
    Chat(ChatEvent),
    Blackbox(BlackboxEvent),
    Anomaly(AnomalyEvent),
    Error(ErrorEvent),
    Log(LogEvent),
}

/// Events emitted by the always-on Blackbox Dual Recording Engine.
///
/// These flow through the normal [`EventBus`] so the UI and any future
/// monitoring consumers can react to recording failures, low disk space, and
/// segment rotation without coupling to the engine internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlackboxEvent {
    Started,
    Stopped,
    SegmentStarted { path: String, index: u64 },
    SegmentClosed { path: String, index: u64, bytes: u64, duration_ms: u64 },
    StorageLow { free_bytes: u64, total_bytes: u64, free_percent: f32 },
    StorageCritical { free_bytes: u64, total_bytes: u64 },
    Stalled { seconds_idle: u64 },
    Recovered { path: String },
    Error { message: String },
    StatusUpdated { status: BlackboxStatus },
}

/// A snapshot of the Blackbox engine's runtime health. Published periodically
/// (and on state changes) via [`BlackboxEvent::StatusUpdated`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlackboxStatus {
    /// Engine is running (worker thread alive).
    pub running: bool,
    /// Engine is actively receiving frames from a capture source.
    pub capturing: bool,
    /// Ingestion is paused because the target disk is critically full.
    pub disk_paused: bool,
    /// Index of the segment currently being written (0-based).
    pub current_segment_index: u64,
    /// Total number of finalized segments since the engine started.
    pub segments_written: u64,
    /// Cumulative encoded bytes written across finalized segments + the active one.
    pub bytes_written: u64,
    /// Frames dropped due to a full channel or disk pause (never blocks the UI).
    pub dropped_frames: u64,
    /// Wall-clock duration the engine has been actively capturing, in ms.
    pub total_duration_ms: u64,
    /// Output path of the segment currently being written, if any.
    pub current_segment_path: Option<String>,
    /// Last non-transient error message, if any.
    pub last_error: Option<String>,
    /// Storage health for the target disk.
    pub storage: BlackboxStorageStatus,
}

/// Disk-space snapshot for the Blackbox output directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlackboxStorageStatus {
    pub free_bytes: u64,
    pub total_bytes: u64,
    /// Percentage of the disk that is free (0.0–100.0). 0.0 when total is unknown.
    pub free_percent: f32,
    pub low_warning: bool,
    pub critical: bool,
}

/// Events emitted by the Short Clip Anomaly Capture engine.
///
/// These flow through the normal [`EventBus`] so the UI (and any future
/// programmatic trigger source) can react to buffer readiness and clip-export
/// results without coupling to the engine internals. The engine is
/// user-toggled (explicit Start/Stop), unlike the always-on Blackbox recorder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnomalyEvent {
    /// The buffer engine is now running and filling its rolling ring.
    Started,
    /// The buffer engine was stopped (user toggle / shutdown).
    Stopped,
    /// The ring has accumulated enough finalized segments to cover the
    /// configured pre-roll window — a clip saved now will include full pre-roll.
    BufferReady { secs_filled: u64 },
    /// A clip was requested (manual button / hotkey / programmatic `save`).
    ClipRequested { clip_id: String },
    /// A clip finished exporting to disk.
    ClipReady { clip_id: String, path: String },
    /// A clip export failed.
    ClipFailed { clip_id: String, message: String },
    /// A clip was requested while another export was already in flight.
    ClipBusy { clip_id: String },
    /// A non-fatal engine error (ffmpeg spawn failure, disk error, ...).
    Error { message: String },
    /// Periodic health snapshot for UI display.
    StatusUpdated { status: AnomalyStatus },
}

/// A snapshot of the Anomaly Capture engine's runtime health. Published
/// periodically (and on state changes) via [`AnomalyEvent::StatusUpdated`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnomalyStatus {
    /// Engine is running (worker thread alive).
    pub running: bool,
    /// Engine has received at least one frame since start.
    pub buffering: bool,
    /// Seconds of footage currently held in the rolling ring (0 until the first
    /// segment finalizes).
    pub buffer_secs_filled: u64,
    /// Total clips successfully exported since the engine started.
    pub clips_exported: u64,
    /// Nonzero while a clip export is in flight (a concurrent `save` is rejected).
    pub clips_busy: u64,
    /// Frames dropped due to a full channel (never blocks the UI).
    pub dropped_frames: u64,
    /// Path of the most recently exported clip, if any.
    pub last_clip_path: Option<String>,
    /// Last non-transient error message, if any.
    pub last_error: Option<String>,
    /// Storage health for the target disk.
    pub storage: AnomalyStorageStatus,
}

/// Disk-space snapshot for the Anomaly output directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnomalyStorageStatus {
    pub free_bytes: u64,
    pub total_bytes: u64,
    /// Percentage of the disk that is free (0.0–100.0). 0.0 when total is unknown.
    pub free_percent: f32,
    pub low_warning: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    Starting,
    Started,
    Stopping,
    Stopped,
    RecordingStarting,
    RecordingStarted,
    RecordingStopping,
    RecordingStopped,
    StreamingStarting { duration_ms: u64 },
    StreamingStarted,
    StreamingStopping,
    StreamingStopped,
    ReplayBufferStarting,
    ReplayBufferStarted,
    ReplayBufferStopping,
    ReplayBufferStopped,
    ReplayBufferSaved { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SourceEvent {
    Created { id: SourceId, name: String, source_type: String },
    Removed { id: SourceId },
    Renamed { id: SourceId, old_name: String, new_name: String },
    Activated { id: SourceId },
    Deactivated { id: SourceId },
    PropertiesChanged { id: SourceId, properties: Vec<String> },
    VideoPropertiesChanged { id: SourceId, info: VideoInfo },
    AudioPropertiesChanged { id: SourceId, info: AudioInfo },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EncoderEvent {
    Created { id: EncoderId, name: String, codec: String },
    Removed { id: EncoderId },
    ParametersChanged { id: EncoderId },
    Error { id: EncoderId, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputEvent {
    Created { id: OutputId, name: String, protocol: String },
    Removed { id: OutputId },
    Connecting { id: OutputId },
    Connected { id: OutputId, server: String },
    Disconnecting { id: OutputId },
    Disconnected { id: OutputId },
    Reconnecting { id: OutputId, attempt: u32 },
    Error { id: OutputId, message: String },
    StatsUpdated { id: OutputId, stats: OutputStats },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputStats {
    pub total_bytes: u64,
    pub total_frames: u64,
    pub bitrate: u32,
    pub frame_rate: f64,
    pub dropped_frames: u64,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SceneEvent {
    Created { id: SceneId, name: String },
    Removed { id: SceneId },
    Renamed { id: SceneId, name: String },
    ItemAdded { scene: SceneId, item: SceneItemId, name: String },
    ItemRemoved { scene: SceneId, item: SceneItemId },
    ItemOrderChanged { scene: SceneId, items: Vec<SceneItemId> },
    ItemTransformChanged { scene: SceneId, item: SceneItemId },
    ItemVisibilityChanged { scene: SceneId, item: SceneItemId, visible: bool },
    CurrentChanged { id: SceneId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProfileEvent {
    Created { id: ProfileId, name: String },
    Removed { id: ProfileId },
    Renamed { id: ProfileId, name: String },
    Switched { id: ProfileId },
    Saved { id: ProfileId },
    Loaded { id: ProfileId },
    Error { id: ProfileId, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChatEvent {
    Connected { platform: String, channel: String },
    Disconnected { platform: String, channel: String },
    Message(ChatMessage),
    UserJoined { platform: String, channel: String, user: String },
    UserLeft { platform: String, channel: String, user: String },
    UserBanned { platform: String, channel: String, user: String, reason: String },
    GiftedSub { platform: String, channel: String, gifter: String, recipient: String, months: u32 },
    Raided { platform: String, channel: String, raider: String, viewers: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub platform: String,
    pub channel: String,
    pub user: String,
    pub user_id: String,
    pub content: String,
    pub timestamp: i64,
    pub color: Option<String>,
    pub badges: Vec<String>,
    pub is_mod: bool,
    pub is_subscriber: bool,
    pub is_vip: bool,
    pub is_broadcaster: bool,
    pub is_first_message: bool,
    pub is_highlighted: bool,
    pub reply_count: u32,
    pub bits: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub code: String,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub level: LogLevel,
    pub message: String,
    pub module: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

pub struct EventBus {
    tx: EventTx,
}

impl EventBus {
    pub fn new() -> (Self, EventRx) {
        let (tx, rx) = unbounded();
        (Self { tx }, rx)
    }
    
    pub fn send(&self, event: RobsEvent) {
        let _ = self.tx.send(event);
    }
    
    pub fn tx(&self) -> EventTx {
        self.tx.clone()
    }
}