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
    Error(ErrorEvent),
    Log(LogEvent),
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