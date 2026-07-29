use crate::aac_encoder::FfmpegAacEncoder;
use crate::ffmpeg_encoder::FfmpegH264Encoder;
use crate::nvenc_encoder::NvencH264Encoder;
use anyhow::Result;
use robs_core::traits::*;

pub struct FfmpegH264Factory;

impl EncoderFactory for FfmpegH264Factory {
    fn encoder_type(&self) -> &str {
        "ffmpeg_h264"
    }
    fn display_name(&self) -> &str {
        "FFmpeg x264 (Software)"
    }
    fn codec_name(&self) -> &str {
        "h264"
    }

    fn create(&self) -> Result<Box<dyn Encoder>> {
        Ok(Box::new(FfmpegH264Encoder::new()))
    }
}

pub struct NvencH264Factory;

impl EncoderFactory for NvencH264Factory {
    fn encoder_type(&self) -> &str {
        "nvenc_h264"
    }
    fn display_name(&self) -> &str {
        "NVIDIA NVENC H.264 (Hardware)"
    }
    fn codec_name(&self) -> &str {
        "h264"
    }

    fn create(&self) -> Result<Box<dyn Encoder>> {
        if !NvencH264Encoder::is_available() {
            return Err(anyhow::anyhow!("NVENC not available. Ensure NVIDIA drivers are installed and FFmpeg supports h264_nvenc."));
        }
        Ok(Box::new(NvencH264Encoder::new()))
    }
}

pub struct FfmpegAacFactory;

impl EncoderFactory for FfmpegAacFactory {
    fn encoder_type(&self) -> &str {
        "ffmpeg_aac"
    }
    fn display_name(&self) -> &str {
        "FFmpeg AAC (Audio)"
    }
    fn codec_name(&self) -> &str {
        "aac"
    }

    fn create(&self) -> Result<Box<dyn Encoder>> {
        if !FfmpegAacEncoder::is_available() {
            return Err(anyhow::anyhow!(
                "AAC encoder not found. Ensure FFmpeg is installed with AAC support."
            ));
        }
        Ok(Box::new(FfmpegAacEncoder::new()))
    }
}

pub fn get_available_video_encoders() -> Vec<Box<dyn EncoderFactory>> {
    let mut encoders: Vec<Box<dyn EncoderFactory>> = Vec::new();

    if NvencH264Encoder::is_available() {
        encoders.push(Box::new(NvencH264Factory));
    }

    encoders.push(Box::new(FfmpegH264Factory));

    encoders
}

pub fn get_available_audio_encoders() -> Vec<Box<dyn EncoderFactory>> {
    let mut encoders: Vec<Box<dyn EncoderFactory>> = Vec::new();

    if FfmpegAacEncoder::is_available() {
        encoders.push(Box::new(FfmpegAacFactory));
    }

    encoders
}

pub fn get_encoder_by_name(name: &str) -> Option<Box<dyn Encoder>> {
    match name {
        "ffmpeg_h264" => Some(Box::new(FfmpegH264Encoder::new())),
        "nvenc_h264" => {
            if NvencH264Encoder::is_available() {
                Some(Box::new(NvencH264Encoder::new()))
            } else {
                None
            }
        }
        "ffmpeg_aac" => {
            if FfmpegAacEncoder::is_available() {
                Some(Box::new(FfmpegAacEncoder::new()))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn detect_encoders() -> EncoderDetection {
    let ffmpeg_available = FfmpegH264Encoder::is_available();
    let nvenc_available = NvencH264Encoder::is_available();
    let aac_available = FfmpegAacEncoder::is_available();
    let gpu_count = NvencH264Encoder::get_gpu_count();

    EncoderDetection {
        nvenc_available,
        aac_available,
        gpu_count,
        ffmpeg_available,
    }
}

#[derive(Debug, Clone)]
pub struct EncoderDetection {
    pub nvenc_available: bool,
    pub aac_available: bool,
    pub gpu_count: u32,
    pub ffmpeg_available: bool,
}

impl EncoderDetection {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.ffmpeg_available {
            parts.push("FFmpeg: available".to_string());
        } else {
            parts.push("FFmpeg: NOT available".to_string());
        }
        if self.nvenc_available {
            parts.push(format!("NVENC: available ({} GPU)", self.gpu_count));
        } else {
            parts.push("NVENC: not available".to_string());
        }
        if self.aac_available {
            parts.push("AAC: available".to_string());
        } else {
            parts.push("AAC: not available".to_string());
        }
        parts.join(", ")
    }
}
