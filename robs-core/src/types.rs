use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId(pub u64);

impl Default for ObjectId {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectId {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        ObjectId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub ObjectId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EncoderId(pub ObjectId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutputId(pub ObjectId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SceneId(pub ObjectId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SceneItemId(pub ObjectId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationId(pub ObjectId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    Video,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelFormat {
    NV12,
    I420,
    I422,
    I444,
    YUY2,
    UYVY,
    RGBA,
    BGRA,
    Rgb24,
    Bgr24,
}

impl PixelFormat {
    pub fn bytes_per_pixel(&self) -> u32 {
        match self {
            PixelFormat::RGBA | PixelFormat::BGRA => 4,
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 3,
            PixelFormat::YUY2 | PixelFormat::UYVY => 2,
            _ => 1,
        }
    }

    pub fn is_planar(&self) -> bool {
        matches!(
            self,
            PixelFormat::NV12 | PixelFormat::I420 | PixelFormat::I422 | PixelFormat::I444
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioFormat {
    U8,
    S16,
    S32,
    F32,
    F64,
}

impl AudioFormat {
    pub fn bytes_per_sample(&self) -> u32 {
        match self {
            AudioFormat::U8 => 1,
            AudioFormat::S16 => 2,
            AudioFormat::S32 | AudioFormat::F32 => 4,
            AudioFormat::F64 => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioSpeaker {
    FL,
    FR,
    FC,
    LFE,
    BL,
    BR,
    SL,
    SR,
}

pub const SPEAKERS_2POINT1: [AudioSpeaker; 3] =
    [AudioSpeaker::FL, AudioSpeaker::FR, AudioSpeaker::LFE];
pub const SPEAKERS_4POINT0: [AudioSpeaker; 4] = [
    AudioSpeaker::FL,
    AudioSpeaker::FR,
    AudioSpeaker::BL,
    AudioSpeaker::BR,
];
pub const SPEAKERS_4POINT1: [AudioSpeaker; 5] = [
    AudioSpeaker::FL,
    AudioSpeaker::FR,
    AudioSpeaker::FC,
    AudioSpeaker::BL,
    AudioSpeaker::BR,
];
pub const SPEAKERS_5POINT1: [AudioSpeaker; 6] = [
    AudioSpeaker::FL,
    AudioSpeaker::FR,
    AudioSpeaker::FC,
    AudioSpeaker::LFE,
    AudioSpeaker::BL,
    AudioSpeaker::BR,
];
pub const SPEAKERS_7POINT1: [AudioSpeaker; 8] = [
    AudioSpeaker::FL,
    AudioSpeaker::FR,
    AudioSpeaker::FC,
    AudioSpeaker::LFE,
    AudioSpeaker::BL,
    AudioSpeaker::BR,
    AudioSpeaker::SL,
    AudioSpeaker::SR,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub format: PixelFormat,
    pub range: VideoRange,
    pub color_space: ColorSpace,
}

impl Default for VideoInfo {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
            format: PixelFormat::NV12,
            range: VideoRange::Partial,
            color_space: ColorSpace::Rec709,
        }
    }
}

impl VideoInfo {
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den as f64
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_micros(1_000_000 * self.fps_den as u64 / self.fps_num as u64)
    }

    pub fn frame_size_bytes(&self, format: PixelFormat) -> usize {
        match format {
            PixelFormat::NV12 => (self.width * self.height * 3 / 2) as usize,
            PixelFormat::I420 => (self.width * self.height * 3 / 2) as usize,
            PixelFormat::I422 => (self.width * self.height * 2) as usize,
            PixelFormat::I444 => (self.width * self.height * 3) as usize,
            PixelFormat::RGBA => (self.width * self.height * 4) as usize,
            PixelFormat::BGRA => (self.width * self.height * 4) as usize,
            PixelFormat::Rgb24 => (self.width * self.height * 3) as usize,
            PixelFormat::Bgr24 => (self.width * self.height * 3) as usize,
            PixelFormat::YUY2 => (self.width * self.height * 2) as usize,
            PixelFormat::UYVY => (self.width * self.height * 2) as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoRange {
    Partial,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorSpace {
    Rec601,
    Rec709,
    Rec2020,
    SRGB,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub format: AudioFormat,
    pub speakers: Vec<AudioSpeaker>,
}

impl Default for AudioInfo {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            format: AudioFormat::F32,
            speakers: SPEAKERS_2POINT1.to_vec(),
        }
    }
}

impl AudioInfo {
    pub fn channels(&self) -> usize {
        self.speakers.len()
    }

    pub fn bytes_per_frame(&self) -> u32 {
        self.format.bytes_per_sample() * self.channels() as u32
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EncoderCapabilities {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub supports_hardware: bool,
    pub supported_pixel_formats: Vec<PixelFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EncoderPreset {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub quality_level: u8,
    pub speed_level: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EncoderRateControl {
    pub bitrate: u32,
    pub buffer_size: Option<u32>,
    pub keyframe_interval: u32,
    pub rate_control_mode: RateControlMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RateControlMode {
    CBR,
    VBR,
    CQP,
    CRF,
}
