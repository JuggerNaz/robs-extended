use anyhow::Result;
use directories::ProjectDirs;
use robs_core::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub video_config: VideoProfileConfig,
    pub audio_config: AudioProfileConfig,
    pub stream_config: StreamingConfig,
    pub output_config: OutputProfileConfig,
    pub sources: Vec<SourceProfileConfig>,
    pub encoders: HashMap<String, EncoderProfileConfig>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: ProfileId(Uuid::new_v4()),
            name: "Untitled".to_string(),
            video_config: VideoProfileConfig::default(),
            audio_config: AudioProfileConfig::default(),
            stream_config: StreamingConfig::default(),
            output_config: OutputProfileConfig::default(),
            sources: Vec::new(),
            encoders: HashMap::new(),
        }
    }
}

impl Profile {
    pub fn new(name: String) -> Self {
        Self {
            id: ProfileId(Uuid::new_v4()),
            name,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoProfileConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub scale_type: ScaleType,
    pub format: PixelFormat,
}

impl Default for VideoProfileConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps_num: 30,
            fps_den: 1,
            output_width: 1280,
            output_height: 720,
            scale_type: ScaleType::Bilinear,
            format: PixelFormat::NV12,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ScaleType {
    Point,
    Bilinear,
    Bicubic,
    Lanczos,
    Area,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioProfileConfig {
    pub sample_rate: u32,
    pub channels: u32,
    pub format: AudioFormat,
    pub tracks: Vec<AudioTrackConfig>,
}

impl Default for AudioProfileConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            format: AudioFormat::F32,
            tracks: vec![AudioTrackConfig::default()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrackConfig {
    pub id: u32,
    pub name: String,
    pub sources: Vec<String>,
    pub mixer: bool,
}

impl Default for AudioTrackConfig {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Track 1".into(),
            sources: Vec::new(),
            mixer: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputProfileConfig {
    pub mode: OutputMode,
    pub recording: RecordingConfig,
    pub streaming: StreamingOutputConfig,
    pub replay_buffer: ReplayBufferConfig,
}

impl Default for OutputProfileConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Simple,
            recording: RecordingConfig::default(),
            streaming: StreamingOutputConfig::default(),
            replay_buffer: ReplayBufferConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OutputMode {
    Simple,
    Advanced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    pub format: String,
    pub path: String,
    pub quality: RecordingQuality,
    pub encoder: String,
    pub bitrate: u32,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            format: "mp4".into(),
            path: "".into(),
            quality: RecordingQuality::High,
            encoder: "x264".into(),
            bitrate: 10000,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RecordingQuality {
    Low,
    Medium,
    High,
    Lossless,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingOutputConfig {
    pub encoder: String,
    pub bitrate: u32,
    pub use_cbr: bool,
    pub enforce_bitrate: bool,
    pub keyint: u32,
    pub preset: String,
}

impl Default for StreamingOutputConfig {
    fn default() -> Self {
        Self {
            encoder: "x264".into(),
            bitrate: 6000,
            use_cbr: true,
            enforce_bitrate: true,
            keyint: 2,
            preset: "faster".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayBufferConfig {
    pub enabled: bool,
    pub duration_secs: u32,
    pub max_mb: u32,
}

impl Default for ReplayBufferConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            duration_secs: 20,
            max_mb: 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingConfig {
    pub destinations: Vec<DestinationConfig>,
    pub primary: Option<String>,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            destinations: Vec::new(),
            primary: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestinationConfig {
    pub name: String,
    pub platform: String,
    pub server: String,
    pub stream_key: String,
    pub enabled: bool,
    pub bandwidth_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProfileConfig {
    pub name: String,
    pub source_type: String,
    pub properties: HashMap<String, serde_json::Value>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderProfileConfig {
    pub name: String,
    pub encoder_type: String,
    pub properties: HashMap<String, serde_json::Value>,
}

pub struct ProfileManager {
    profiles: HashMap<ProfileId, Profile>,
    current: Option<ProfileId>,
    profiles_dir: PathBuf,
}

impl ProfileManager {
    pub fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("ai", "robs", "ROBS")
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;

        let profiles_dir = dirs.config_dir().join("profiles");
        fs::create_dir_all(&profiles_dir)?;

        let mut manager = Self {
            profiles: HashMap::new(),
            current: None,
            profiles_dir,
        };

        manager.load_all()?;
        Ok(manager)
    }

    pub fn load_all(&mut self) -> Result<()> {
        if !self.profiles_dir.exists() {
            fs::create_dir_all(&self.profiles_dir)?;
            return Ok(());
        }

        for entry in fs::read_dir(&self.profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension() == Some(std::ffi::OsStr::new("toml")) {
                if let Ok(profile) = self.load_profile(&path) {
                    self.profiles.insert(profile.id, profile);
                } else if let Ok(profile) = self.load_json_profile(&path) {
                    self.profiles.insert(profile.id, profile);
                }
            }
        }

        Ok(())
    }

    fn load_profile(&self, path: &Path) -> Result<Profile> {
        let content = fs::read_to_string(path)?;
        let profile: Profile = toml::from_str(&content)?;
        Ok(profile)
    }

    fn load_json_profile(&self, path: &Path) -> Result<Profile> {
        let content = fs::read_to_string(path)?;
        let profile: Profile = serde_json::from_str(&content)?;
        Ok(profile)
    }

    pub fn create(&mut self, name: String) -> ProfileId {
        let profile = Profile::new(name);
        let id = profile.id;
        self.profiles.insert(id, profile);
        id
    }

    pub fn delete(&mut self, id: ProfileId) -> Result<()> {
        if let Some(profile) = self.profiles.remove(&id) {
            let filename = format!("{}.toml", profile.name);
            let path = self.profiles_dir.join(filename);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn get(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.get(&id)
    }

    pub fn get_mut(&mut self, id: ProfileId) -> Option<&mut Profile> {
        self.profiles.get_mut(&id)
    }

    pub fn current(&self) -> Option<&Profile> {
        self.current.and_then(|id| self.profiles.get(&id))
    }

    pub fn current_mut(&mut self) -> Option<&mut Profile> {
        self.current.and_then(move |id| self.profiles.get_mut(&id))
    }

    pub fn set_current(&mut self, id: ProfileId) -> Result<()> {
        if self.profiles.contains_key(&id) {
            self.current = Some(id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Profile not found"))
        }
    }

    pub fn save(&self, id: ProfileId) -> Result<()> {
        if let Some(profile) = self.profiles.get(&id) {
            let filename = format!("{}.toml", profile.name);
            let path = self.profiles_dir.join(filename);
            let content = toml::to_string_pretty(profile)?;
            fs::write(path, content)?;
            println!("[Profile] Saved: {}", profile.name);
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<(ProfileId, String)> {
        self.profiles
            .values()
            .map(|p| (p.id, p.name.clone()))
            .collect()
    }

    pub fn duplicate(&mut self, id: ProfileId, new_name: String) -> Result<ProfileId> {
        let profile = self
            .profiles
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("Profile not found"))?;
        let mut new_profile = profile.clone();
        new_profile.id = ProfileId(Uuid::new_v4());
        new_profile.name = new_name;
        let new_id = new_profile.id;
        self.profiles.insert(new_id, new_profile);
        Ok(new_id)
    }
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self {
            profiles: HashMap::new(),
            current: None,
            profiles_dir: PathBuf::new(),
        })
    }
}
