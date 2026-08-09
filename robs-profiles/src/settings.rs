use serde::{Deserialize, Serialize};

/// Settings for the always-on Blackbox Dual Recording Engine. Local-only for
/// now; the `cloud` field is reserved so a future cloud-archive sink can be
/// configured without a schema migration.
///
/// See `robs-outputs::blackbox::BlackboxConfig` for the runtime struct these
/// are projected into.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboxSettings {
    /// Master switch. When false, the engine never starts.
    pub enabled: bool,
    /// Output directory. Empty => a `Blackbox/` subdir under the configured
    /// recording path (resolved by the UI).
    pub output_dir: String,
    /// Rotate a segment after this many seconds of capture.
    pub segment_duration_secs: u64,
    /// Rotate a segment after roughly this many MiB of raw input.
    pub segment_size_mb: u64,
    /// ffmpeg encoder id: `"libx264"` (software) or `"h264_nvenc"` (hardware).
    pub encoder: String,
    /// Container. Fixed to mkv for crash-safety; kept configurable for parity
    /// with the main recorder's settings shape.
    pub container: String,
    /// CRF used by the software (x264) path.
    pub crf: u8,
    /// Bitrate in kbps used by the nvenc CBR path.
    pub video_bitrate_kbps: u32,
    /// Emit a `StorageLow` warning when free disk drops below this percent.
    pub disk_low_warn_percent: u8,
    /// Pause ingestion when free disk drops below this percent.
    pub disk_low_critical_percent: u8,
    /// Hard cap on total on-disk blackbox storage in GiB (0 = unlimited).
    pub max_retention_gb: u32,
    /// Emit a `Stalled` event after this many seconds with no incoming frames.
    pub stall_threshold_secs: u64,
    /// Reserved for a future cloud-archive sink. Ignored today.
    pub cloud: serde_json::Value,
}

impl Default for BlackboxSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            output_dir: String::new(),
            segment_duration_secs: 900, // 15 min
            segment_size_mb: 512,
            encoder: "libx264".into(),
            container: "mkv".into(),
            crf: 28,
            video_bitrate_kbps: 2500,
            disk_low_warn_percent: 10,
            disk_low_critical_percent: 3,
            max_retention_gb: 10,
            stall_threshold_secs: 10,
            cloud: serde_json::Value::Null,
        }
    }
}

impl BlackboxSettings {
    pub fn load_or_default() -> Self {
        Self::default()
    }
}

impl AppSettings {
    pub fn load_or_default() -> Self {
        Self::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub general: GeneralSettings,
    pub video: VideoSettings,
    pub audio: AudioSettings,
    pub hotkeys: Vec<HotkeyBinding>,
    pub ui: UiSettings,
    pub blackbox: BlackboxSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            general: GeneralSettings::default(),
            video: VideoSettings::default(),
            audio: AudioSettings::default(),
            hotkeys: Vec::new(),
            ui: UiSettings::default(),
            blackbox: BlackboxSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    pub language: String,
    pub theme: String,
    pub check_for_updates: bool,
    pub confirm_on_exit: bool,
    pub minimize_to_tray: bool,
    pub always_on_top: bool,
    pub recording_prefix: String,
    pub recording_suffix: String,
    pub replay_buffer_prefix: String,
    pub replay_buffer_suffix: String,
    pub filename_formatting: String,
    pub overwrite_confirm: bool,
    pub auto_replay_buffer: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            language: "en".into(),
            theme: "dark".into(),
            check_for_updates: true,
            confirm_on_exit: true,
            minimize_to_tray: false,
            always_on_top: false,
            recording_prefix: "".into(),
            recording_suffix: "".into(),
            replay_buffer_prefix: "".into(),
            replay_buffer_suffix: "".into(),
            filename_formatting: "%CCYY-%MM-%DD %hh-%mm-%ss".into(),
            overwrite_confirm: true,
            auto_replay_buffer: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoSettings {
    pub adapter: u32,
    pub vsync: bool,
    pub fps: u32,
    pub base_resolution: (u32, u32),
    pub output_resolution: (u32, u32),
    pub downscale_filter: String,
    pub disable_audio_monitoring: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            adapter: 0,
            vsync: true,
            fps: 30,
            base_resolution: (1920, 1080),
            output_resolution: (1280, 720),
            downscale_filter: "bilinear".into(),
            disable_audio_monitoring: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    pub monitoring_device: String,
    pub monitoring_device_name: String,
    pub disable_audio_ducking: bool,
    pub suppress_warning: bool,
    pub sample_rate: u32,
    pub channel_setup: String,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            monitoring_device: "default".into(),
            monitoring_device_name: "Default".into(),
            disable_audio_ducking: false,
            suppress_warning: false,
            sample_rate: 48000,
            channel_setup: "Stereo".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    pub layout: String,
    pub preview_enabled: bool,
    pub preview_scaling: String,
    pub dock_layout: DockLayout,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            layout: "default".into(),
            preview_enabled: true,
            preview_scaling: "fit".into(),
            dock_layout: DockLayout::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockLayout {
    pub docks: Vec<DockNode>,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            docks: vec![
                DockNode::pane("Sources", 0.0, 0.0, 0.25, 0.4),
                DockNode::pane("Scenes", 0.0, 0.4, 0.25, 0.3),
                DockNode::pane("Controls", 0.75, 0.7, 0.25, 0.3),
                DockNode::pane("Chat", 0.75, 0.0, 0.25, 0.7),
                DockNode::tabbed(vec!["Audio Mixer", "Chat"], 0.75, 0.0, 0.25, 0.7),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockNode {
    pub kind: DockKind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub tabs: Vec<String>,
    pub children: Vec<DockNode>,
}

impl DockNode {
    pub fn pane(name: &str, x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            kind: DockKind::Pane,
            x,
            y,
            w,
            h,
            tabs: vec![name.to_string()],
            children: Vec::new(),
        }
    }

    pub fn tabbed(names: Vec<&str>, x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            kind: DockKind::Tabs,
            x,
            y,
            w,
            h,
            tabs: names.into_iter().map(String::from).collect(),
            children: Vec::new(),
        }
    }

    pub fn horizontal(children: Vec<DockNode>) -> Self {
        Self {
            kind: DockKind::Horizontal,
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            tabs: Vec::new(),
            children,
        }
    }

    pub fn vertical(children: Vec<DockNode>) -> Self {
        Self {
            kind: DockKind::Vertical,
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            tabs: Vec::new(),
            children,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DockKind {
    Horizontal,
    Vertical,
    Pane,
    Tabs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub action: String,
    pub key: String,
    pub modifiers: Vec<String>,
}

impl HotkeyBinding {
    pub fn new(action: &str, key: &str, modifiers: Vec<&str>) -> Self {
        Self {
            action: action.to_string(),
            key: key.to_string(),
            modifiers: modifiers.into_iter().map(String::from).collect(),
        }
    }
}
