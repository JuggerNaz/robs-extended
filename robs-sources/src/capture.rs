use robs_core::traits::*;
use robs_core::*;
use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
#[cfg(windows)]
use std::process::{Command, Stdio};

/// Monitor capture source using FFmpeg gdigrab
pub struct MonitorCaptureSource {
    id: SourceId,
    name: String,
    monitor_index: u32,
    active: bool,
    video_info: VideoInfo,
    ffmpeg_process: Option<std::process::Child>,
    frame_count: u64,
}

impl MonitorCaptureSource {
    pub fn new(name: String, monitor_index: u32) -> Self {
        let width = 1920;
        let height = 1080;
        Self {
            id: SourceId(ObjectId::new()),
            name,
            monitor_index,
            active: false,
            video_info: VideoInfo {
                width,
                height,
                fps_num: 30,
                fps_den: 1,
                format: PixelFormat::BGRA,
                range: VideoRange::Full,
                color_space: ColorSpace::SRGB,
            },
            ffmpeg_process: None,
            frame_count: 0,
        }
    }

    fn start_capture(&mut self) -> Result<()> {
        #[cfg(windows)]
        {
            let input = "desktop".to_string();

            let mut cmd = Command::new("ffmpeg");
            cmd.args([
                "-f", "gdigrab",
                "-framerate", "30",
                "-draw_mouse", "1",
                "-i", &input,
                "-f", "rawvideo",
                "-pix_fmt", "bgra",
                "-",
            ]);

            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::null());

            let child = cmd.spawn()?;
            self.ffmpeg_process = Some(child);
            self.frame_count = 0;

            println!("[MonitorCapture] Started FFmpeg gdigrab for monitor {}", self.monitor_index);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!(
                "gdigrab capture is Windows-only; screen capture on this platform \
                 needs a native backend (not implemented yet)"
            );
        }
    }

    fn stop_capture(&mut self) {
        if let Some(mut child) = self.ffmpeg_process.take() {
            let _ = child.kill();
        }
        self.frame_count = 0;
        println!("[MonitorCapture] Stopped capture");
    }
}

#[async_trait]
impl Source for MonitorCaptureSource {
    fn id(&self) -> SourceId { self.id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn get_video_info(&self) -> Option<VideoInfo> { Some(self.video_info) }
    fn get_audio_info(&self) -> Option<AudioInfo> { None }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    async fn activate(&mut self) -> Result<()> {
        self.active = true;
        self.start_capture()?;
        println!("[MonitorCapture] Activated: Monitor {}", self.monitor_index);
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<()> {
        self.active = false;
        self.stop_capture();
        println!("[MonitorCapture] Deactivated");
        Ok(())
    }

    fn is_active(&self) -> bool { self.active }

    fn properties_definition(&self) -> Vec<PropertyDef> {
        vec![
            PropertyDef {
                name: "monitor".into(),
                display_name: "Monitor".into(),
                type_: PropertyType::Int,
                default: PropertyValue::Int(self.monitor_index as i64),
                min: Some(0.0),
                max: Some(10.0),
                ..Default::default()
            },
        ]
    }

    fn get_property(&self, name: &str) -> Option<PropertyValue> {
        match name {
            "monitor" => Some(PropertyValue::Int(self.monitor_index as i64)),
            _ => None,
        }
    }

    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()> {
        match name {
            "monitor" => {
                if let PropertyValue::Int(m) = value {
                    self.monitor_index = m as u32;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl VideoSource for MonitorCaptureSource {
    async fn get_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.active {
            return Ok(None);
        }

        // Read from FFmpeg process
        if let Some(ref mut child) = self.ffmpeg_process {
            if let Some(stdout) = child.stdout.as_mut() {
                let width = self.video_info.width as usize;
                let height = self.video_info.height as usize;
                let frame_size = width * height * 4; // BGRA

                let mut buffer = vec![0u8; frame_size];
                use std::io::Read;
                match stdout.read_exact(&mut buffer) {
                    Ok(_) => {
                        self.frame_count += 1;
                        let pts = (self.frame_count * 1000 / 30) as i64;
                        return Ok(Some(VideoFrame {
                            width: self.video_info.width,
                            height: self.video_info.height,
                            format: self.video_info.format,
                            data: buffer,
                            pts,
                            duration: 33333,
                            linesize: vec![],
                        }));
                    }
                    Err(_) => {
                        // Frame not ready yet
                        return Ok(None);
                    }
                }
            }
        }

        // Return empty frame if no data
        let frame = VideoFrame::new(self.video_info.width, self.video_info.height, self.video_info.format);
        Ok(Some(frame))
    }
}

/// Window capture source using FFmpeg gdigrab
pub struct WindowCaptureSource {
    id: SourceId,
    name: String,
    window_title: String,
    active: bool,
    video_info: VideoInfo,
    ffmpeg_process: Option<std::process::Child>,
    frame_count: u64,
}

impl WindowCaptureSource {
    pub fn new(name: String, window_title: String) -> Self {
        Self {
            id: SourceId(ObjectId::new()),
            name,
            window_title,
            active: false,
            video_info: VideoInfo {
                width: 1920,
                height: 1080,
                fps_num: 30,
                fps_den: 1,
                format: PixelFormat::BGRA,
                range: VideoRange::Full,
                color_space: ColorSpace::SRGB,
            },
            ffmpeg_process: None,
            frame_count: 0,
        }
    }

    fn start_capture(&mut self) -> Result<()> {
        #[cfg(windows)]
        {
            let input = format!("title={}", self.window_title);

            let mut cmd = Command::new("ffmpeg");
            cmd.args([
                "-f", "gdigrab",
                "-framerate", "30",
                "-offset_x", "0",
                "-offset_y", "0",
                "-i", &input,
                "-f", "rawvideo",
                "-pix_fmt", "bgra",
                "-",
            ]);

            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::null());

            let child = cmd.spawn()?;
            self.ffmpeg_process = Some(child);
            self.frame_count = 0;

            println!("[WindowCapture] Started FFmpeg gdigrab for window: {}", self.window_title);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!(
                "gdigrab window capture is Windows-only; window capture on this \
                 platform needs a native backend (not implemented yet)"
            );
        }
    }

    fn stop_capture(&mut self) {
        if let Some(mut child) = self.ffmpeg_process.take() {
            let _ = child.kill();
        }
        self.frame_count = 0;
        println!("[WindowCapture] Stopped capture");
    }
}

#[async_trait]
impl Source for WindowCaptureSource {
    fn id(&self) -> SourceId { self.id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn get_video_info(&self) -> Option<VideoInfo> { Some(self.video_info) }
    fn get_audio_info(&self) -> Option<AudioInfo> { None }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    async fn activate(&mut self) -> Result<()> {
        self.active = true;
        self.start_capture()?;
        println!("[WindowCapture] Activated: {}", self.window_title);
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<()> {
        self.active = false;
        self.stop_capture();
        println!("[WindowCapture] Deactivated");
        Ok(())
    }

    fn is_active(&self) -> bool { self.active }

    fn properties_definition(&self) -> Vec<PropertyDef> {
        vec![
            PropertyDef {
                name: "window".into(),
                display_name: "Window Title".into(),
                type_: PropertyType::String,
                default: PropertyValue::String(self.window_title.clone()),
                ..Default::default()
            },
        ]
    }

    fn get_property(&self, name: &str) -> Option<PropertyValue> {
        match name {
            "window" => Some(PropertyValue::String(self.window_title.clone())),
            _ => None,
        }
    }

    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()> {
        match name {
            "window" => {
                if let PropertyValue::String(w) = value {
                    self.window_title = w;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl VideoSource for WindowCaptureSource {
    async fn get_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.active {
            return Ok(None);
        }

        if let Some(ref mut child) = self.ffmpeg_process {
            if let Some(stdout) = child.stdout.as_mut() {
                let width = self.video_info.width as usize;
                let height = self.video_info.height as usize;
                let frame_size = width * height * 4;

                let mut buffer = vec![0u8; frame_size];
                use std::io::Read;
                match stdout.read_exact(&mut buffer) {
                    Ok(_) => {
                        self.frame_count += 1;
                        let pts = (self.frame_count * 1000 / 30) as i64;
                        return Ok(Some(VideoFrame {
                            width: self.video_info.width,
                            height: self.video_info.height,
                            format: self.video_info.format,
                            data: buffer,
                            pts,
                            duration: 33333,
                            linesize: vec![],
                        }));
                    }
                    Err(_) => return Ok(None),
                }
            }
        }

        Ok(None)
    }
}

/// Test pattern source for debugging
pub struct TestPatternSource {
    id: SourceId,
    name: String,
    active: bool,
    video_info: VideoInfo,
    frame_count: u64,
}

impl TestPatternSource {
    pub fn new(name: String) -> Self {
        Self {
            id: SourceId(ObjectId::new()),
            name,
            active: false,
            video_info: VideoInfo {
                width: 1280,
                height: 720,
                fps_num: 30,
                fps_den: 1,
                format: PixelFormat::BGRA,
                range: VideoRange::Full,
                color_space: ColorSpace::SRGB,
            },
            frame_count: 0,
        }
    }
}

#[async_trait]
impl Source for TestPatternSource {
    fn id(&self) -> SourceId { self.id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn get_video_info(&self) -> Option<VideoInfo> { Some(self.video_info) }
    fn get_audio_info(&self) -> Option<AudioInfo> { None }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    async fn activate(&mut self) -> Result<()> {
        self.active = true;
        self.frame_count = 0;
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<()> {
        self.active = false;
        Ok(())
    }

    fn is_active(&self) -> bool { self.active }
    fn properties_definition(&self) -> Vec<PropertyDef> { vec![] }
    fn get_property(&self, _name: &str) -> Option<PropertyValue> { None }
    fn set_property(&mut self, _name: &str, _value: PropertyValue) -> Result<()> { Ok(()) }
}

#[async_trait]
impl VideoSource for TestPatternSource {
    async fn get_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.active {
            return Ok(None);
        }

        self.frame_count += 1;
        let width = self.video_info.width as usize;
        let height = self.video_info.height as usize;
        let mut data = vec![0u8; width * height * 4];

        // Generate moving gradient pattern
        let phase = (self.frame_count % 120) as f32 / 120.0 * std::f32::consts::PI * 2.0;
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * 4;
                let r = ((x as f32 / width as f32 * 255.0) + phase.sin() * 50.0) as u8;
                let g = ((y as f32 / height as f32 * 255.0) + phase.cos() * 50.0) as u8;
                let b = 128u8;
                data[idx] = b;     // B
                data[idx + 1] = g; // G
                data[idx + 2] = r; // R
                data[idx + 3] = 255; // A
            }
        }

        Ok(Some(VideoFrame {
            width: self.video_info.width,
            height: self.video_info.height,
            format: self.video_info.format,
            data,
            pts: (self.frame_count * 1000 / 30) as i64,
            duration: 33333,
            linesize: vec![],
        }))
    }
}
