use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Settings for the Short Clip Anomaly Capture engine.
///
/// An explicitly user-toggled rolling buffer that, on a manual or programmatic
/// trigger, exports a short MP4 clip spanning a configurable pre-roll +
/// post-roll window. See `robs-outputs::anomaly::AnomalyConfig` for the runtime
/// struct these are projected into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Missing fields deserialize to the struct defaults, so a partial or
/// hand-edited `anomaly` section still loads.
#[serde(default)]
pub struct AnomalySettings {
    /// Master switch. Unlike the always-on Blackbox recorder, the buffer must
    /// be explicitly started by the user (Start/Stop).
    pub enabled: bool,
    /// Output directory for exported clips. Empty => an `Anomaly/` subdir under
    /// the configured recording path (resolved by the UI).
    pub output_dir: String,
    /// Footage captured *before* the trigger to include in the exported clip.
    pub pre_roll_secs: u32,
    /// Footage captured *after* the trigger to include in the exported clip.
    pub post_roll_secs: u32,
    /// Hard cap on the on-disk scratch ring size in MiB.
    pub max_buffer_mb: u32,
    /// Duration of each rolling scratch segment. Shorter = finer pre-roll
    /// accuracy + faster concat; longer = fewer ffmpeg spawns.
    pub segment_duration_secs: u32,
    /// ffmpeg encoder id: `"libx264"` (software) or `"h264_nvenc"` (hardware).
    pub encoder: String,
    /// CRF used by the software (x264) path.
    pub crf: u8,
    /// Bitrate in kbps used by the nvenc CBR path.
    pub video_bitrate_kbps: u32,
    /// Output (scaled) width. 0 = use the native capture resolution.
    pub output_width: u32,
    /// Output (scaled) height. 0 = use the native capture resolution.
    pub output_height: u32,
    /// Prefix prepended to exported clip filenames.
    pub clip_prefix: String,
    /// Suffix appended to exported clip filenames (before the extension).
    pub clip_suffix: String,
    /// Hotkey binding string for the manual Capture Clip action (free-form;
    /// matched on raw key text, e.g. "Ctrl+Shift+A"). Empty = unbound.
    pub capture_clip_hotkey: String,
}

impl Default for AnomalySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            output_dir: String::new(),
            pre_roll_secs: 15,
            post_roll_secs: 15,
            max_buffer_mb: 512,
            segment_duration_secs: 2,
            encoder: "libx264".into(),
            crf: 23,
            video_bitrate_kbps: 2500,
            output_width: 0,
            output_height: 0,
            clip_prefix: "Anomaly".into(),
            clip_suffix: String::new(),
            capture_clip_hotkey: String::new(),
        }
    }
}

impl AnomalySettings {
    /// Load the anomaly section from the canonical settings file, falling
    /// back to defaults when it is absent. A file that exists but cannot be
    /// parsed (or has no `anomaly` key) warns on stderr and yields defaults —
    /// a bad hand-edit must never keep the app from starting.
    pub fn load_or_default() -> Self {
        let Some(path) = settings_file_path() else {
            return Self::default();
        };
        match Self::load_from(&path) {
            Some(settings) => settings,
            None => {
                if path.exists() {
                    eprintln!(
                        "[Settings] anomaly settings unreadable, using defaults: {}",
                        path.display()
                    );
                }
                Self::default()
            }
        }
    }

    /// Persist the anomaly section to the canonical settings file.
    pub fn save(&self) -> Result<()> {
        let path = settings_file_path().context("could not determine the config directory")?;
        self.save_to(&path)
    }

    /// Read the `anomaly` section of the JSON object at `path`.
    /// `None` when the file is missing, invalid JSON, or has no `anomaly`
    /// key. Unknown fields inside the section are ignored so forward-added
    /// settings don't brick older builds.
    pub fn load_from(path: &Path) -> Option<Self> {
        let content = fs::read_to_string(path).ok()?;
        let root: serde_json::Value = serde_json::from_str(&content).ok()?;
        serde_json::from_value(root.get("anomaly")?.clone()).ok()
    }

    /// Write the anomaly section into the JSON object at `path`, creating the
    /// file (and parent directories) as needed. The file is a plain JSON
    /// object keyed by section (`{"anomaly": {...}}`) so further sections
    /// (blackbox, general, ...) can be added later without a migration; any
    /// unrelated sections already present are preserved. An existing file
    /// that fails to parse is replaced rather than propagated as an error —
    /// saving current settings should always win over a corrupt file.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let mut root: serde_json::Value = fs::read_to_string(path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let obj = root
            .as_object_mut()
            .context("settings file root is not a JSON object")?;
        obj.insert("anomaly".into(), serde_json::to_value(self)?);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(&root)?)?;
        Ok(())
    }
}

/// Canonical settings file: `<config_dir>/settings.json`, sibling of the
/// `profiles/` directory `ProfileManager` uses (same `ProjectDirs` root).
pub fn settings_file_path() -> Option<PathBuf> {
    ProjectDirs::from("ai", "robs", "ROBS").map(|d| d.config_dir().join("settings.json"))
}

impl AppSettings {
    pub fn load_or_default() -> Self {
        Self::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettings {
    pub general: GeneralSettings,
    pub video: VideoSettings,
    pub audio: AudioSettings,
    pub hotkeys: Vec<HotkeyBinding>,
    pub ui: UiSettings,
    pub blackbox: BlackboxSettings,
    pub anomaly: AnomalySettings,
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
