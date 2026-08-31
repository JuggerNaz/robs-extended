use robs_core::traits::*;
use robs_core::*;
use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::process::{Command, Stdio};
use std::io::Read;

/// FFmpeg input format for audio capture on the current platform
/// (DirectShow on Windows, AVFoundation on macOS, PulseAudio on Linux).
fn audio_input_format() -> &'static str {
    #[cfg(windows)]
    {
        "dshow"
    }
    #[cfg(target_os = "macos")]
    {
        "avfoundation"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "pulse"
    }
    #[cfg(not(any(windows, unix)))]
    {
        "dshow"
    }
}

/// Build the FFmpeg `-i` input specifier for an audio device on the current
/// platform. `device` is the app-level device id; on Windows those are
/// `audio=<name>` DirectShow specs, on macOS AVFoundation names/indices.
/// `mic` only affects the Windows empty-id fallback (mic vs desktop loopback).
fn audio_input_spec(device: &str, mic: bool) -> String {
    let _ = mic;
    #[cfg(windows)]
    {
        if device.is_empty() {
            if mic {
                "audio=0".to_string()
            } else {
                "audio=virtual-audio-capturer".to_string()
            }
        } else if device.starts_with("audio=") {
            device.to_string()
        } else {
            format!("audio={}", device)
        }
    }
    #[cfg(target_os = "macos")]
    {
        // avfoundation takes a device name or index; `:<index>` selects an
        // audio-only input. Strip Windows-style `audio=` prefixes from
        // persisted settings.
        let name = device.strip_prefix("audio=").unwrap_or(device);
        if name.is_empty() || name == "default" || name == "disabled" {
            ":0".to_string()
        } else {
            name.to_string()
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let name = device.strip_prefix("audio=").unwrap_or(device);
        if name.is_empty() || name == "default" || name == "disabled" {
            "default".to_string()
        } else {
            name.to_string()
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        device.to_string()
    }
}

/// System audio capture using FFmpeg dsound (DirectSound) or wasapi
pub struct SystemAudioSource {
    id: SourceId,
    name: String,
    device_id: String,
    active: bool,
    audio_info: AudioInfo,
    ffmpeg_process: Option<std::process::Child>,
}

impl SystemAudioSource {
    pub fn new(name: String, device_id: String) -> Self {
        Self {
            id: SourceId(ObjectId::new()),
            name,
            device_id,
            active: false,
            audio_info: AudioInfo {
                sample_rate: 48000,
                format: AudioFormat::S16,
                speakers: vec![AudioSpeaker::FL, AudioSpeaker::FR],
            },
            ffmpeg_process: None,
        }
    }

    fn start_capture(&mut self) -> Result<()> {
        let device = audio_input_spec(&self.device_id, false);

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-f", audio_input_format(),
            "-i", &device,
            "-f", "s16le",
            "-ar", "48000",
            "-ac", "2",
            "-",
        ]);

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let child = cmd.spawn()?;
        self.ffmpeg_process = Some(child);

        println!("[SystemAudio] Started capture from: {}", self.device_id);
        Ok(())
    }

    fn stop_capture(&mut self) {
        if let Some(mut child) = self.ffmpeg_process.take() {
            let _ = child.kill();
        }
        println!("[SystemAudio] Stopped capture");
    }
}

#[async_trait]
impl Source for SystemAudioSource {
    fn id(&self) -> SourceId { self.id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn get_video_info(&self) -> Option<VideoInfo> { None }
    fn get_audio_info(&self) -> Option<AudioInfo> { Some(self.audio_info.clone()) }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    async fn activate(&mut self) -> Result<()> {
        self.active = true;
        self.start_capture()?;
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<()> {
        self.active = false;
        self.stop_capture();
        Ok(())
    }

    fn is_active(&self) -> bool { self.active }

    fn properties_definition(&self) -> Vec<PropertyDef> {
        vec![
            PropertyDef {
                name: "device".into(),
                display_name: "Audio Device".into(),
                type_: PropertyType::String,
                default: PropertyValue::String(self.device_id.clone()),
                ..Default::default()
            },
        ]
    }

    fn get_property(&self, name: &str) -> Option<PropertyValue> {
        match name {
            "device" => Some(PropertyValue::String(self.device_id.clone())),
            _ => None,
        }
    }

    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()> {
        match name {
            "device" => {
                if let PropertyValue::String(d) = value {
                    self.device_id = d;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl AudioSource for SystemAudioSource {
    async fn get_audio(&mut self, frames: u32) -> Result<Option<AudioFrame>> {
        if !self.active {
            return Ok(None);
        }

        if let Some(ref mut child) = self.ffmpeg_process {
            if let Some(stdout) = child.stdout.as_mut() {
                let bytes_per_sample = 2 * 2; // 16-bit stereo
                let frame_size = frames as usize * bytes_per_sample;
                let mut buffer = vec![0u8; frame_size];

                match stdout.read_exact(&mut buffer) {
                    Ok(_) => {
                        return Ok(Some(AudioFrame {
                            sample_rate: self.audio_info.sample_rate,
                            format: self.audio_info.format,
                            speakers: self.audio_info.speakers.clone(),
                            data: buffer,
                            frames,
                            pts: 0,
                        }));
                    }
                    Err(_) => return Ok(None),
                }
            }
        }

        Ok(None)
    }
}

/// Microphone/line-in audio source
pub struct MicrophoneSource {
    id: SourceId,
    name: String,
    device_name: String,
    active: bool,
    audio_info: AudioInfo,
    ffmpeg_process: Option<std::process::Child>,
}

impl MicrophoneSource {
    pub fn new(name: String, device_name: String) -> Self {
        Self {
            id: SourceId(ObjectId::new()),
            name,
            device_name,
            active: false,
            audio_info: AudioInfo {
                sample_rate: 48000,
                format: AudioFormat::S16,
                speakers: vec![AudioSpeaker::FL, AudioSpeaker::FR],
            },
            ffmpeg_process: None,
        }
    }

    fn start_capture(&mut self) -> Result<()> {
        let input = audio_input_spec(&self.device_name, true);

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-f", audio_input_format(),
            "-i", &input,
            "-f", "s16le",
            "-ar", "48000",
            "-ac", "2",
            "-",
        ]);

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let child = cmd.spawn()?;
        self.ffmpeg_process = Some(child);

        println!("[Microphone] Started capture from: {}", self.device_name);
        Ok(())
    }

    fn stop_capture(&mut self) {
        if let Some(mut child) = self.ffmpeg_process.take() {
            let _ = child.kill();
        }
    }
}

#[async_trait]
impl Source for MicrophoneSource {
    fn id(&self) -> SourceId { self.id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn get_video_info(&self) -> Option<VideoInfo> { None }
    fn get_audio_info(&self) -> Option<AudioInfo> { Some(self.audio_info.clone()) }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    async fn activate(&mut self) -> Result<()> {
        self.active = true;
        self.start_capture()?;
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<()> {
        self.active = false;
        self.stop_capture();
        Ok(())
    }

    fn is_active(&self) -> bool { self.active }

    fn properties_definition(&self) -> Vec<PropertyDef> {
        vec![
            PropertyDef {
                name: "device".into(),
                display_name: "Microphone Device".into(),
                type_: PropertyType::String,
                default: PropertyValue::String(self.device_name.clone()),
                ..Default::default()
            },
        ]
    }

    fn get_property(&self, name: &str) -> Option<PropertyValue> {
        match name {
            "device" => Some(PropertyValue::String(self.device_name.clone())),
            _ => None,
        }
    }

    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()> {
        match name {
            "device" => {
                if let PropertyValue::String(d) = value {
                    self.device_name = d;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl AudioSource for MicrophoneSource {
    async fn get_audio(&mut self, frames: u32) -> Result<Option<AudioFrame>> {
        if !self.active {
            return Ok(None);
        }

        let frame_size = frames as usize * 4; // 16-bit stereo
        let mut buffer = vec![0u8; frame_size];

        if let Some(ref mut child) = self.ffmpeg_process {
            if let Some(stdout) = child.stdout.as_mut() {
                use std::io::Read;
                match stdout.read_exact(&mut buffer) {
                    Ok(_) => {
                        return Ok(Some(AudioFrame {
                            sample_rate: self.audio_info.sample_rate,
                            format: self.audio_info.format,
                            speakers: self.audio_info.speakers.clone(),
                            data: buffer,
                            frames,
                            pts: 0,
                        }));
                    }
                    Err(_) => return Ok(None),
                }
            }
        }

        Ok(None)
    }
}

/// List available audio devices using FFmpeg
pub fn list_audio_devices() -> Vec<(String, String)> {
    let mut devices = Vec::new();

    // List audio devices via the platform's FFmpeg input format
    if let Ok(output) = Command::new("ffmpeg")
        .args(["-list_devices", "true", "-f", audio_input_format(), "-i", ""])
        .stderr(std::process::Stdio::piped())
        .output()
    {
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr.lines() {
            if line.contains("DirectShow audio devices") || line.contains("Audio") {
                // Device names follow in subsequent lines
            }
            // Parse device names from FFmpeg output
            // Format: "[dshow @ ...]  \"Device Name\""
            if line.contains("\"") {
                if let Some(start) = line.find("\"") {
                    if let Some(end) = line[start+1..].find("\"") {
                        let name = &line[start+1..start+1+end];
                        if !name.contains("Device") && !name.is_empty() {
                            devices.push((name.to_string(), name.to_string()));
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            // FFmpeg >= 8 avfoundation lists unquoted devices:
            // `[AVFoundation indev @ ...] [0] Device Name`
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut in_audio_section = false;

            for line in stderr.lines() {
                if line.contains("audio devices") {
                    in_audio_section = true;
                    continue;
                }

                if line.contains("video devices") {
                    in_audio_section = false;
                }

                if !in_audio_section {
                    continue;
                }

                let payload = line.rsplit_once("] ").map(|(_, rest)| rest).unwrap_or(line);
                if let Some(rest) = payload.strip_prefix('[') {
                    if let Some((_, name)) = rest.split_once(']') {
                        let name = name.trim();
                        if !name.is_empty() {
                            devices.push((name.to_string(), name.to_string()));
                        }
                    }
                }
            }
        }
    }

    // Fallback: add common device names
    if devices.is_empty() {
        devices.push(("default".to_string(), "Default".to_string()));
    }

    devices
}
