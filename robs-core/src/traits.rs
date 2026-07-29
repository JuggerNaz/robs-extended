use crate::types::*;
use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;
use serde::Serialize;

#[async_trait]
pub trait Source: Send + Sync {
    fn id(&self) -> SourceId;
    fn name(&self) -> &str;
    fn set_name(&mut self, name: String);
    
    fn get_video_info(&self) -> Option<VideoInfo>;
    fn get_audio_info(&self) -> Option<AudioInfo>;
    
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    
    async fn activate(&mut self) -> Result<()>;
    async fn deactivate(&mut self) -> Result<()>;
    
    fn is_active(&self) -> bool;
    
    fn properties_definition(&self) -> Vec<PropertyDef>;
    fn get_property(&self, name: &str) -> Option<PropertyValue>;
    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()>;
}

#[async_trait]
pub trait VideoSource: Source {
    async fn get_frame(&mut self) -> Result<Option<VideoFrame>>;
}

#[async_trait]
pub trait AudioSource: Source {
    async fn get_audio(&mut self, frames: u32) -> Result<Option<AudioFrame>>;
}

#[async_trait]
pub trait Encoder: Send + Sync {
    fn id(&self) -> EncoderId;
    fn name(&self) -> &str;
    fn codec_name(&self) -> &str;
    fn media_type(&self) -> MediaType;
    
    fn get_input_info(&self) -> Option<MediaInfo>;
    fn get_output_info(&self) -> Option<MediaInfo>;
    
    fn get_caps(&self) -> EncoderCapabilities;
    fn get_presets(&self) -> Vec<EncoderPreset>;
    fn get_current_preset(&self) -> EncoderPreset;
    fn set_preset(&mut self, preset: EncoderPreset) -> Result<()>;
    
    fn parameters_definition(&self) -> Vec<PropertyDef>;
    fn get_parameter(&self, name: &str) -> Option<PropertyValue>;
    fn set_parameter(&mut self, name: &str, value: PropertyValue) -> Result<()>;
    
    async fn initialize(&mut self, input: MediaInfo, output: MediaInfo) -> Result<()>;
    async fn encode(&mut self, input: MediaData) -> Result<Option<EncodedPacket>>;
    async fn flush(&mut self) -> Result<Vec<EncodedPacket>>;
    
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[derive(Debug, Clone)]
pub enum MediaInfo {
    Video(VideoEncodeInfo),
    Audio(AudioEncodeInfo),
}

#[derive(Debug, Clone)]
pub struct VideoEncodeInfo {
    pub video: VideoInfo,
    pub encoder_name: String,
    pub codec_params: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct AudioEncodeInfo {
    pub audio: AudioInfo,
    pub encoder_name: String,
    pub codec_params: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum MediaData {
    Video(VideoFrame),
    Audio(AudioFrame),
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    pub pts: i64,
    pub duration: i64,
    pub linesize: Vec<usize>,
}

impl VideoFrame {
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        let linesize = Self::calculate_linesize(width, height, format);
        let size = linesize.iter().sum();
        Self {
            width,
            height,
            format,
            data: vec![0u8; size],
            pts: 0,
            duration: 0,
            linesize,
        }
    }
    
    fn calculate_linesize(width: u32, _height: u32, format: PixelFormat) -> Vec<usize> {
        match format {
            PixelFormat::RGBA | PixelFormat::BGRA => vec![(width * 4) as usize],
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => vec![(width * 3) as usize],
            PixelFormat::YUY2 | PixelFormat::UYVY => vec![(width * 2) as usize],
            PixelFormat::NV12 => vec![
                (width) as usize,
                (width) as usize,
            ],
            PixelFormat::I420 => vec![
                (width) as usize,
                (width / 2) as usize,
                (width / 2) as usize,
            ],
            _ => vec![(width) as usize],
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub sample_rate: u32,
    pub format: AudioFormat,
    pub speakers: Vec<AudioSpeaker>,
    pub data: Vec<u8>,
    pub frames: u32,
    pub pts: i64,
}

impl AudioFrame {
    pub fn new(frames: u32, audio_info: &AudioInfo) -> Self {
        let bytes_per_sample = audio_info.format.bytes_per_sample() as usize;
        let channels = audio_info.channels();
        let size = frames as usize * channels * bytes_per_sample;
        Self {
            sample_rate: audio_info.sample_rate,
            format: audio_info.format,
            speakers: audio_info.speakers.clone(),
            data: vec![0u8; size],
            frames,
            pts: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
    pub keyframe: bool,
    pub track: TrackId,
}

#[async_trait]
pub trait Output: Send + Sync {
    fn id(&self) -> OutputId;
    fn name(&self) -> &str;
    fn protocol(&self) -> &str;
    
    fn is_connected(&self) -> bool;
    fn is_reconnecting(&self) -> bool;
    
    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn send_packet(&mut self, packet: EncodedPacket) -> Result<()>;
    
    fn properties_definition(&self) -> Vec<PropertyDef>;
    fn get_property(&self, name: &str) -> Option<PropertyValue>;
    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()>;
    
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub trait SourceFactory: Send + Sync {
    fn source_type(&self) -> &str;
    fn display_name(&self) -> &str;
    fn create(&self) -> Result<Box<dyn Source>>;
    fn properties_definition(&self) -> Vec<PropertyDef>;
}

pub trait EncoderFactory: Send + Sync {
    fn encoder_type(&self) -> &str;
    fn display_name(&self) -> &str;
    fn codec_name(&self) -> &str;
    fn create(&self) -> Result<Box<dyn Encoder>>;
}

pub trait OutputFactory: Send + Sync {
    fn output_type(&self) -> &str;
    fn display_name(&self) -> &str;
    fn protocol(&self) -> &str;
    fn create(&self) -> Result<Box<dyn Output>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub enum PropertyType {
    Bool,
    Int,
    Float,
    String,
    Enum,
    Path,
    Color,
    Font,
    Object,
}

#[derive(Debug, Clone)]
pub struct PropertyDef {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub type_: PropertyType,
    pub default: PropertyValue,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub enum_values: Vec<(String, String)>,
    pub visible: bool,
    pub enabled: bool,
}

impl Default for PropertyDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: String::new(),
            description: String::new(),
            type_: PropertyType::String,
            default: PropertyValue::String(String::new()),
            min: None,
            max: None,
            step: None,
            enum_values: Vec::new(),
            visible: true,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Enum(String),
    Path(String),
    Color(u32),
    Font(FontInfo),
    Object(serde_json::Value),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontInfo {
    pub family: String,
    pub size: u32,
    pub bold: bool,
    pub italic: bool,
}

pub type SourcePtr = Arc<parking_lot::RwLock<Box<dyn Source>>>;
pub type EncoderPtr = Arc<parking_lot::RwLock<Box<dyn Encoder>>>;
pub type OutputPtr = Arc<parking_lot::RwLock<Box<dyn Output>>>;