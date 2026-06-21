use crate::dxgi_capture::DxgiCaptureManager;
use eframe::egui;
use parking_lot::RwLock;
use robs_chat::aggregator::ChatAggregator;
use robs_chat::message::{ChatEvent, UnifiedChatMessage};
use robs_core::scene::{Crop, Position, Scale};
use robs_core::traits::VideoSource;
use robs_core::types::{SceneItemId, SourceId};
use robs_core::SceneCollection;
use robs_encoding::detect_encoders;
use robs_outputs::FileOutput;
use robs_profiles::profile::ProfileManager;
use robs_sources::native_capture::get_open_windows;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
struct MonitorInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub position_x: i32,
    pub position_y: i32,
}

fn get_monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();

    // Use Windows GDI to enumerate display monitors
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::{LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
        };

        unsafe extern "system" fn enum_monitors(
            hmonitor: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            lparam: LPARAM,
        ) -> windows::Win32::Foundation::BOOL {
            let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);

            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

            if GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
                let is_primary = (info.monitorInfo.dwFlags & 1) != 0;
                let width = info.monitorInfo.rcMonitor.right - info.monitorInfo.rcMonitor.left;
                let height = info.monitorInfo.rcMonitor.bottom - info.monitorInfo.rcMonitor.top;
                let position_x = info.monitorInfo.rcMonitor.left;
                let position_y = info.monitorInfo.rcMonitor.top;

                let name = String::from_utf16_lossy(
                    &info.szDevice[..info
                        .szDevice
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(info.szDevice.len())],
                );

                monitors.push(MonitorInfo {
                    name,
                    width: width as u32,
                    height: height as u32,
                    is_primary,
                    position_x,
                    position_y,
                });
            }

            windows::Win32::Foundation::BOOL(1)
        }

        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(enum_monitors),
                LPARAM(&mut monitors as *mut _ as isize),
            );
        }
    }

    // If no monitors found, provide a default
    if monitors.is_empty() {
        monitors.push(MonitorInfo {
            name: "Display 1".to_string(),
            width: 1920,
            height: 1080,
            is_primary: true,
            position_x: 0,
            position_y: 0,
        });
    }

    monitors
}

#[derive(Clone)]
struct AudioDeviceInfo {
    pub name: String,
    pub id: String,
    pub is_input: bool, // true = microphone/aux, false = desktop audio/speakers
}

fn get_audio_devices() -> Vec<AudioDeviceInfo> {
    let mut devices = Vec::new();

    // Add special options first
    devices.push(AudioDeviceInfo {
        name: "Disabled".to_string(),
        id: "disabled".to_string(),
        is_input: true,
    });
    devices.push(AudioDeviceInfo {
        name: "Default".to_string(),
        id: "default".to_string(),
        is_input: true,
    });

    // Use FFmpeg to list DirectShow audio devices
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;

        // Get audio input devices (microphones)
        if let Ok(output) = Command::new("ffmpeg")
            .args(["-list_devices", "true", "-f", "dshow", "-i", "dummy"])
            .stderr(std::process::Stdio::piped())
            .output()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut in_audio_section = false;

            for line in stderr.lines() {
                if line.contains("DirectShow audio devices") {
                    in_audio_section = true;
                    continue;
                }

                // Stop at video devices section
                if line.contains("DirectShow video devices") {
                    in_audio_section = false;
                }

                if in_audio_section && line.contains("(audio)") {
                    // Parse device name from format: "Device Name" (audio)
                    if let Some(start) = line.find("\"") {
                        if let Some(end) = line[start + 1..].find("\"") {
                            let name = &line[start + 1..start + 1 + end];
                            // Skip device enumeration lines, keep actual device names
                            if !name.contains("Device") && !name.is_empty() && name.len() > 2 {
                                let id = format!("audio={}", name);
                                devices.push(AudioDeviceInfo {
                                    name: name.to_string(),
                                    id: id.clone(),
                                    is_input: true,
                                });
                            }
                        }
                    }
                }
            }
        }

        // If no devices found via FFmpeg, add known devices as fallback
        if devices.len() <= 2 {
            // Add GoXLR broadcast mix (system audio)
            devices.push(AudioDeviceInfo {
                name: "Broadcast Stream Mix (TC-HELICON GoXLR)".to_string(),
                id: "audio=Broadcast Stream Mix (TC-HELICON GoXLR)".to_string(),
                is_input: false, // This is system/desktop audio
            });
            // Add GoXLR chat mic
            devices.push(AudioDeviceInfo {
                name: "Chat Mic (TC-HELICON GoXLR)".to_string(),
                id: "audio=Chat Mic (TC-HELICON GoXLR)".to_string(),
                is_input: true,
            });
            // Add VB-Audio Virtual Cable
            devices.push(AudioDeviceInfo {
                name: "CABLE Output (VB-Audio Virtual Cable)".to_string(),
                id: "audio=CABLE Output (VB-Audio Virtual Cable)".to_string(),
                is_input: false,
            });
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        devices.push(AudioDeviceInfo {
            name: "Default".to_string(),
            id: "default".to_string(),
            is_input: true,
        });
    }

    devices
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Panel {
    Preview,
    Sources,
    Scenes,
    Controls,
    AudioMixer,
    Chat,
    Stats,
}

pub struct RobsApp {
    streaming: bool,
    recording: bool,
    recording_paused: bool,
    streaming_paused: bool,
    profile_manager: Arc<RwLock<ProfileManager>>,
    chat_messages: Arc<RwLock<VecDeque<UnifiedChatMessage>>>,
    chat_input: String,
    chat_rx: Option<mpsc::Receiver<ChatEvent>>,
    current_scene: String,
    scenes: SceneCollection, // Migrated to SceneCollection
    show_settings: bool,
    settings_rect: Option<egui::Rect>,
    active_settings_tab: usize,
    audio_channels: Vec<AudioChannel>,
    streaming_time: u64,
    recording_time: u64,
    bitrate: u32,
    dropped_frames: u64,
    fps: f64,
    active_panel: Panel,
    show_preview: bool,
    show_scenes: bool,
    show_controls: bool,
    show_audio: bool,
    show_chat: bool,
    show_stats: bool,
    confirm_on_exit: bool,
    minimize_to_tray: bool,
    always_on_top: bool,
    check_for_updates: bool,
    filename_formatting: String,
    base_width: u32,
    base_height: u32,
    output_width: u32,
    output_height: u32,
    fps_setting: f32,
    stream_server: String,
    stream_key: String,
    stream_bitrate: u32,
    keyframe_interval: u32,
    recording_bitrate: u32,
    recording_path: String,
    recording_format: String,
    video_encoder: String,
    audio_encoder: String,
    audio_sample_rate: String,  // "44100" or "48000"
    audio_channel_mode: String, // "mono" or "stereo"
    available_video_encoders: Vec<String>,
    available_audio_encoders: Vec<String>,
    // Audio devices
    audio_devices: Vec<AudioDeviceInfo>,
    selected_audio_device: String,
    nvenc_available: bool,
    aac_available: bool,
    ffmpeg_available: bool,
    recording_file_output: Option<FileOutput>,
    recording_start_time: Option<u64>,
    last_recording_path: String,
    ffmpeg_recording_handle: Option<std::process::Child>, // Store FFmpeg process handle for graceful shutdown
    recording_dxgi_thread: Option<std::thread::JoinHandle<()>>, // DXGI capture thread for recording
    recording_stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>, // Signal to stop DXGI thread
    recording_frame_sender: Option<std::sync::mpsc::Sender<Vec<u8>>>, // Channel to send frames to FFmpeg
    recording_ffmpeg_stdin: Option<std::process::ChildStdin>,         // FFmpeg stdin handle
    // Video capture / Preview
    active_video_source: Option<Box<dyn VideoSource>>,
    preview_textures: HashMap<SceneItemId, egui::TextureHandle>, // One texture per source
    frame_buffer: HashMap<String, Vec<u8>>,
    // Direct DXGI Desktop Duplication capture (GPU-accelerated)
    // Replaces dxgi-capture-rs with direct implementation for full monitor control
    dxgi_manager: Option<DxgiCaptureManager>, // Our own DXGI capture manager
    // Real-time preview capture (per-source tracking)
    preview_capture_active: bool,
    preview_frame_sender: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    preview_capture_handle: Option<std::process::Child>, // Store FFmpeg process for preview
    preview_frame_receiver: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    // Native preview capture state
    preview_frame_count: u64,
    last_preview_capture: std::time::Instant,
    // Source properties modal state
    show_source_properties: bool,
    editing_source_id: Option<SceneItemId>,
    editing_source_name: String,
    editing_source_pos_x: f32,
    editing_source_pos_y: f32,
    editing_source_scale_x: f32,
    editing_source_scale_y: f32,
    editing_source_rotation: f32,
    editing_source_crop_left: u32,
    editing_source_crop_top: u32,
    editing_source_crop_right: u32,
    editing_source_crop_bottom: u32,
    // Annotation / mark-up tools
    show_annotations: bool,
    annotations: Vec<robs_core::Annotation>,
    annotation_tool: robs_core::AnnotationTool,
    annotation_style: robs_core::AnnotationStyle,
    annotation_drawing: Option<robs_core::Annotation>,
    selected_annotation: Option<robs_core::AnnotationId>,
    // Text annotation inline editor
    editing_text_id: Option<robs_core::AnnotationId>,
    text_input: String,
    // Font used for baking text annotations into recordings (lazy-loaded).
    record_font: Option<ab_glyph::FontVec>,
}

#[derive(Clone)]
struct AudioChannel {
    name: String,
    volume: f32,
    muted: bool,
    device_id: String, // device ID or "disabled" or "default"
    is_desktop: bool,  // true = desktop audio, false = mic/aux
}

/// Parse monitor coordinates from a display capture source name
/// Format: "Display Capture - Name|idx:X|x:Y|y:Z|w:W|h:H"
/// Returns (x, y, width, height) in virtual screen coordinates
fn parse_monitor_coords(source_name: &str) -> (i32, i32, i32, i32) {
    // Default to primary monitor at 1920x1080 if parsing fails
    let mut cap_x = 0i32;
    let mut cap_y = 0i32;
    let mut cap_w = 1920i32;
    let mut cap_h = 1080i32;

    // Try to parse the extended format
    if let Some(pipe_pos) = source_name.find('|') {
        let params = &source_name[pipe_pos + 1..];
        for param in params.split('|') {
            if let Some(colon_pos) = param.find(':') {
                let key = &param[..colon_pos];
                let value = &param[colon_pos + 1..];
                match key {
                    "x" => cap_x = value.parse().unwrap_or(0),
                    "y" => cap_y = value.parse().unwrap_or(0),
                    "w" => cap_w = value.parse().unwrap_or(1920),
                    "h" => cap_h = value.parse().unwrap_or(1080),
                    _ => {}
                }
            }
        }
    }

    (cap_x, cap_y, cap_w, cap_h)
}

/// Parse monitor index from a display capture source name
/// Format: "Display Capture - Name|idx:X|x:Y|y:Z|w:W|h:H"
/// Returns monitor index (0 = primary, 1+ = secondary)
fn parse_monitor_index(source_name: &str) -> u32 {
    if let Some(pipe_pos) = source_name.find('|') {
        let params = &source_name[pipe_pos + 1..];
        for param in params.split('|') {
            if let Some(colon_pos) = param.find(':') {
                let key = &param[..colon_pos];
                let value = &param[colon_pos + 1..];
                if key == "idx" {
                    return value.parse().unwrap_or(0);
                }
            }
        }
    }
    0 // Default to primary monitor
}

impl RobsApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
        });

        let detection = detect_encoders();

        let mut video_encoders = Vec::new();
        if detection.ffmpeg_available {
            video_encoders.push("FFmpeg x264 (Software)".into());
        }
        if detection.nvenc_available {
            video_encoders.push("NVIDIA NVENC H.264 (Hardware)".into());
        }
        if video_encoders.is_empty() {
            video_encoders.push("None Available".into());
        }

        let mut audio_encoders = Vec::new();
        if detection.aac_available {
            audio_encoders.push("FFmpeg AAC".into());
        }
        if audio_encoders.is_empty() {
            audio_encoders.push("None Available".into());
        }

        let video_encoder = if detection.nvenc_available {
            "NVIDIA NVENC H.264 (Hardware)".into()
        } else if detection.ffmpeg_available {
            "FFmpeg x264 (Software)".into()
        } else {
            "None Available".into()
        };

        let audio_encoder = if detection.aac_available {
            "FFmpeg AAC".into()
        } else {
            "None Available".into()
        };

        Self {
            streaming: false,
            recording: false,
            recording_paused: false,
            streaming_paused: false,
            profile_manager: Arc::new(RwLock::new(ProfileManager::default())),
            chat_messages: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            chat_input: String::new(),
            chat_rx: None,
            current_scene: "Main Scene".to_string(),
            scenes: {
                let mut col = SceneCollection::new();
                col.create_scene("Main Scene".to_string());
                col.set_current_scene("Main Scene");
                col
            },
            show_settings: false,
            settings_rect: None,
            audio_channels: vec![
                AudioChannel {
                    name: "Mic/Aux".into(),
                    volume: 0.8,
                    muted: false,
                    device_id: "default".to_string(),
                    is_desktop: false,
                },
                AudioChannel {
                    name: "Desktop Audio".into(),
                    volume: 0.6,
                    muted: false,
                    device_id: "default".to_string(),
                    is_desktop: true,
                },
            ],
            streaming_time: 0,
            recording_time: 0,
            bitrate: 6000,
            dropped_frames: 0,
            fps: 30.0,
            active_panel: Panel::Preview,
            show_preview: true,
            show_scenes: true,
            show_controls: true,
            show_audio: true,
            show_chat: true,
            show_stats: true,
            active_settings_tab: 0,
            confirm_on_exit: true,
            minimize_to_tray: false,
            always_on_top: false,
            check_for_updates: true,
            filename_formatting: "%CCYY-%MM-%DD %hh-%mm-%ss".into(),
            base_width: 1920,
            base_height: 1080,
            output_width: 1280,
            output_height: 720,
            fps_setting: 30.0,
            stream_server: "rtmp://live.twitch.tv/app".into(),
            stream_key: String::new(),
            stream_bitrate: 6000,
            keyframe_interval: 2,
            recording_bitrate: 10000,
            recording_path: String::new(),
            recording_format: "mp4".into(),
            video_encoder,
            audio_encoder,
            audio_sample_rate: "48000".to_string(),
            audio_channel_mode: "stereo".to_string(),
            available_video_encoders: video_encoders,
            available_audio_encoders: audio_encoders,
            // Audio devices
            audio_devices: get_audio_devices(),
            selected_audio_device: "0".to_string(),
            nvenc_available: detection.nvenc_available,
            aac_available: detection.aac_available,
            ffmpeg_available: detection.ffmpeg_available,
            recording_file_output: None,
            recording_start_time: None,
            last_recording_path: String::new(),
            ffmpeg_recording_handle: None,
            recording_dxgi_thread: None,
            recording_stop_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            recording_frame_sender: None,
            recording_ffmpeg_stdin: None,
            active_video_source: None,
            preview_textures: HashMap::new(),
            frame_buffer: HashMap::new(),
            // Direct DXGI Desktop Duplication capture
            dxgi_manager: None, // Initialized lazily on first capture
            // Real-time preview
            preview_capture_active: false,
            preview_frame_sender: None,
            preview_capture_handle: None,
            preview_frame_receiver: None,
            // Native preview capture
            preview_frame_count: 0,
            last_preview_capture: std::time::Instant::now(),
            // Source properties modal
            show_source_properties: false,
            editing_source_id: None,
            editing_source_name: String::new(),
            editing_source_pos_x: 0.0,
            editing_source_pos_y: 0.0,
            editing_source_scale_x: 1.0,
            editing_source_scale_y: 1.0,
            editing_source_rotation: 0.0,
            editing_source_crop_left: 0,
            editing_source_crop_top: 0,
            editing_source_crop_right: 0,
            editing_source_crop_bottom: 0,
            // Annotation / mark-up tools
            show_annotations: true,
            annotations: Vec::new(),
            annotation_tool: robs_core::AnnotationTool::default(),
            annotation_style: robs_core::AnnotationStyle::default(),
            annotation_drawing: None,
            selected_annotation: None,
            editing_text_id: None,
            text_input: String::new(),
            record_font: None,
        }
    }

    pub fn with_chat(
        mut self,
        _aggregator: Arc<ChatAggregator>,
        rx: mpsc::Receiver<ChatEvent>,
    ) -> Self {
        self.chat_rx = Some(rx);
        self
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &mut self.chat_rx {
            while let Ok(event) = rx.try_recv() {
                if let ChatEvent::Message(msg) = event {
                    self.chat_messages.write().push_back(*msg);
                }
            }
        }
        if self.streaming {
            if !self.streaming_paused {
                self.streaming_time += 1;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if self.recording {
            if !self.recording_paused {
                self.recording_time += 1;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn start_recording(&mut self) {
        // Force stderr output to be visible
        use std::io::Write;
        let _ = std::io::stderr().write_all(b"[Recording] start_recording() called\n");

        let path = if self.recording_path.is_empty() {
            let default_dir = std::env::var("USERPROFILE")
                .map(|p| format!("{}\\Videos", p))
                .unwrap_or_else(|_| "C:\\Users\\Videos".to_string());
            fs::create_dir_all(&default_dir).ok();
            default_dir
        } else {
            self.recording_path.clone()
        };

        let timestamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
        let filename = format!("ROBS_{}.{}", timestamp, self.recording_format);
        let full_path = PathBuf::from(&path).join(&filename);

        // Create parent directory if needed
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).ok();
        }

        self.last_recording_path = full_path.to_string_lossy().into_owned();

        // Find the active capture source from current scene
        eprintln!("[Recording] Looking for capture source...");
        if let Some(scene) = self.scenes.current_scene() {
            eprintln!("[Recording] Total items in scene: {}", scene.item_count());
            for (i, item) in scene.items().iter().enumerate() {
                eprintln!(
                    "[Recording] Item {}: name='{}' visible={}",
                    i,
                    item.name(),
                    item.is_visible()
                );
            }
        }

        // Check for window_capture first, then fall back to monitor_capture
        // Now using SceneCollection instead of flat sources list
        let scene = self.scenes.current_scene();

        let (input_spec, offset_x, offset_y, video_width, video_height, use_window_capture) =
            if let Some(scene) = scene {
                // Find first visible window_capture item
                let window_item = scene
                    .items()
                    .iter()
                    .find(|i| i.is_visible() && i.name().starts_with("Window:"));

                if let Some(item) = window_item {
                    let window_title = item.name().strip_prefix("Window: ").unwrap_or(item.name());
                    eprintln!("[Recording] Found window_capture source: {}", window_title);
                    (format!("title={}", window_title), 0, 0, 0, 0, true)
                } else {
                    // Check for monitor capture
                    let monitor_item = scene
                        .items()
                        .iter()
                        .find(|i| i.is_visible() && i.name().starts_with("Display Capture"));

                    if let Some(item) = monitor_item {
                        // Use position coordinates from the source name
                        // Format: "Display Capture - Name|idx:X|x:Y|y:Z|w:W|h:H"
                        let (mon_x, mon_y, mon_w, mon_h) = parse_monitor_coords(item.name());

                        eprintln!("[Recording] Found monitor_capture source: {}", item.name());
                        eprintln!(
                            "[Recording] Using monitor at ({},{}) - {}x{}",
                            mon_x, mon_y, mon_w, mon_h
                        );

                        let input = "desktop".to_string();
                        (input, mon_x, mon_y, mon_w, mon_h, false)
                    } else {
                        eprintln!("[Recording] No capture source found in scene!");
                        ("desktop".to_string(), 0, 0, 1920, 1080, false)
                    }
                }
            } else {
                eprintln!("[Recording] No current scene!");
                ("desktop".to_string(), 0, 0, 1920, 1080, false)
            };

        // If we found a window capture, use window dimensions (0 means we'll get them from the window)
        let final_width = if use_window_capture { 0 } else { video_width };
        let final_height = if use_window_capture { 0 } else { video_height };

        eprintln!(
            "[Recording] Using input: {} at offset ({}, {}) size {}x{}",
            input_spec, offset_x, offset_y, video_width, video_height
        );

        let output_path = self.last_recording_path.clone();

        // Determine which encoder to use
        let encoder =
            if self.video_encoder.contains("NVENC") || self.video_encoder.contains("NVIDIA") {
                "h264_nvenc"
            } else {
                "libx264"
            };

        // Build FFmpeg args based on format
        let format = self.recording_format.clone();
        let mut ffmpeg_args = Vec::new();

        // Check if we're doing monitor capture via DXGI (no gdigrab)
        let use_dxgi_recording = !use_window_capture;

        if use_dxgi_recording {
            // DXGI recording: pipe raw frames via stdin
            // Frames are already scaled to output resolution on the main thread
            let output_w = self.output_width;
            let output_h = self.output_height;

            eprintln!(
                "[Recording] Using DXGI recording pipeline: sending {}x{} frames to FFmpeg",
                output_w, output_h
            );

            // Tell FFmpeg to expect raw BGRA frames at OUTPUT resolution (already scaled)
            ffmpeg_args.push("-f".into());
            ffmpeg_args.push("rawvideo".into());
            ffmpeg_args.push("-pix_fmt".into());
            ffmpeg_args.push("bgra".into());
            ffmpeg_args.push("-video_size".into());
            ffmpeg_args.push(format!("{}x{}", output_w, output_h));
            ffmpeg_args.push("-framerate".into());
            ffmpeg_args.push(self.fps_setting.to_string());
            ffmpeg_args.push("-i".into());
            ffmpeg_args.push("pipe:0".into());
        } else {
            // Window capture fallback: use gdigrab
            ffmpeg_args.push("-f".into());
            ffmpeg_args.push("gdigrab".into());
            ffmpeg_args.push("-framerate".into());
            ffmpeg_args.push(self.fps_setting.to_string());
            ffmpeg_args.push("-draw_mouse".into());
            ffmpeg_args.push("1".into());

            if use_window_capture {
                ffmpeg_args.push("-i".into());
                ffmpeg_args.push(input_spec.clone());
            } else {
                ffmpeg_args.push("-offset_x".into());
                ffmpeg_args.push(offset_x.to_string());
                ffmpeg_args.push("-offset_y".into());
                ffmpeg_args.push(offset_y.to_string());
                ffmpeg_args.push("-video_size".into());
                ffmpeg_args.push(format!("{}x{}", final_width, final_height));
                ffmpeg_args.push("-i".into());
                ffmpeg_args.push(input_spec.clone());
            }
        }

        // NOTE: Audio disabled for now - dshow audio is a live stream that never ends,
        // causing FFmpeg to hang indefinitely. Will re-add with proper -shortest handling.

        // Video codec
        ffmpeg_args.push("-c:v".into());
        ffmpeg_args.push(encoder.to_string());

        // NVENC-specific settings (matching OBS: CBR, HQ tuning, 2-pass)
        if encoder == "h264_nvenc" {
            ffmpeg_args.push("-preset".into());
            ffmpeg_args.push("p4".into());
            ffmpeg_args.push("-tune".into());
            ffmpeg_args.push("hq".into()); // High Quality (OBS default, not low-latency)
            ffmpeg_args.push("-gpu".into());
            ffmpeg_args.push("0".into());
            ffmpeg_args.push("-rc".into());
            ffmpeg_args.push("cbr".into());
            ffmpeg_args.push("-b:v".into());
            ffmpeg_args.push(format!("{}k", self.recording_bitrate));
            ffmpeg_args.push("-maxrate".into());
            ffmpeg_args.push(format!("{}k", self.recording_bitrate));
            ffmpeg_args.push("-bufsize".into());
            ffmpeg_args.push(format!("{}k", self.recording_bitrate * 2));
            // Keyframe interval (gop size = fps * keyframe_interval)
            let gop_size = (self.fps_setting * self.keyframe_interval as f32) as u32;
            ffmpeg_args.push("-g".into());
            ffmpeg_args.push(gop_size.to_string());
            // Two-pass multipass encoding (quarter resolution for first pass)
            ffmpeg_args.push("-multipass".into());
            ffmpeg_args.push("fullres".into());
        } else {
            // x264 settings
            ffmpeg_args.push("-preset".into());
            ffmpeg_args.push("fast".into());
            ffmpeg_args.push("-tune".into());
            ffmpeg_args.push("zerolatency".into());
            ffmpeg_args.push("-crf".into());
            ffmpeg_args.push("23".into());
        }

        // Frame rate
        ffmpeg_args.push("-r".into());
        ffmpeg_args.push(self.fps_setting.to_string());

        // Output format and overwrite
        if format == "mp4" {
            ffmpeg_args.push("-f".into());
            ffmpeg_args.push("mp4".into());
        } else if format == "mkv" {
            ffmpeg_args.push("-f".into());
            ffmpeg_args.push("matroska".into());
        } else if format == "flv" {
            ffmpeg_args.push("-f".into());
            ffmpeg_args.push("flv".into());
        }
        ffmpeg_args.push("-y".into());
        ffmpeg_args.push(output_path.clone());

        eprintln!("[Recording] FFmpeg args: {:?}", ffmpeg_args);

        // Spawn FFmpeg to capture the selected monitor
        let ffmpeg_result = std::process::Command::new("ffmpeg")
            .args(&ffmpeg_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match ffmpeg_result {
            Ok(mut child) => {
                let pid = child.id();
                eprintln!(
                    "[Recording] FFmpeg started (PID: {}) capturing to {}",
                    pid, output_path
                );

                // For DXGI recording: spawn a thread to capture frames and pipe to FFmpeg
                if use_dxgi_recording {
                    let stop_flag = self.recording_stop_flag.clone();
                    let position = (offset_x, offset_y);
                    let input_width = video_width as u32;
                    let input_height = video_height as u32;
                    let fps = self.fps_setting;

                    // For DXGI recording: spawn a writer thread that reads frames from a channel
                    // and pipes them to FFmpeg stdin. The main thread captures frames via existing DXGI.
                    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
                    self.recording_frame_sender = Some(tx);

                    let stop_flag = self.recording_stop_flag.clone();
                    let stdin = child.stdin.take().expect("FFmpeg stdin should be piped");
                    let output_w = self.output_width;
                    let output_h = self.output_height;
                    let needs_scaling =
                        video_width as u32 != output_w || video_height as u32 != output_h;

                    eprintln!(
                        "[Recording] Starting FFmpeg stdin writer thread (scaling: {}, output: {}x{})",
                        needs_scaling, output_w, output_h
                    );

                    let thread_handle = std::thread::spawn(move || {
                        let mut stdin = stdin;
                        let mut frame_count: u64 = 0;
                        let mut total_bytes: u64 = 0;

                        while !stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                                Ok(frame_data) => {
                                    let bytes = frame_data.len();
                                    total_bytes += bytes as u64;

                                    if let Err(e) =
                                        std::io::Write::write_all(&mut stdin, &frame_data)
                                    {
                                        eprintln!(
                                            "[DXGI-Record] Failed to write frame to FFmpeg: {}",
                                            e
                                        );
                                        break;
                                    }
                                    frame_count += 1;
                                    if frame_count % 10 == 0 {
                                        eprintln!(
                                            "[DXGI-Record] Written {} frames to FFmpeg ({} MB total)",
                                            frame_count,
                                            total_bytes / (1024 * 1024)
                                        );
                                    }
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                    continue;
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                    eprintln!("[DXGI-Record] Channel disconnected, exiting");
                                    break;
                                }
                            }
                        }

                        eprintln!(
                            "[DXGI-Record] Writer thread exiting after {} frames ({} MB)",
                            frame_count,
                            total_bytes / (1024 * 1024)
                        );
                    });

                    self.recording_dxgi_thread = Some(thread_handle);
                } else {
                    // Non-DXGI recording: just drop stdin (gdigrab doesn't need it)
                    drop(child.stdin.take());
                }

                // Try to read initial stderr output to check for errors
                if let Some(stderr) = child.stderr.take() {
                    use std::io::{BufRead, BufReader};
                    let reader = BufReader::new(stderr);
                    // Read first few lines of stderr
                    let mut lines = Vec::new();
                    for line in reader.lines().take(10) {
                        if let Ok(l) = line {
                            lines.push(l);
                        }
                    }
                    if !lines.is_empty() {
                        println!("[Recording] FFmpeg output: {}", lines.join("; "));
                    }
                }

                // Store the handle for graceful shutdown
                eprintln!("[Recording] Storing FFmpeg handle, PID: {}", child.id());
                self.ffmpeg_recording_handle = Some(child);
                eprintln!(
                    "[Recording] Handle stored: {:?}",
                    self.ffmpeg_recording_handle.is_some()
                );
            }
            Err(e) => {
                println!("[Recording] Failed to start FFmpeg: {}", e);
            }
        }

        self.recording = true;
        self.recording_paused = false;
        self.recording_time = 0;
        self.recording_start_time = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
    }

    fn stop_recording(&mut self) {
        eprintln!("[Recording] Stopping recording...");

        // 1. Signal the writer thread to stop
        self.recording_stop_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // 2. Drop the sender to close the channel - this signals the writer thread
        self.recording_frame_sender = None;

        // 3. Wait for the writer thread to finish (it will drop FFmpeg stdin = EOF)
        if let Some(handle) = self.recording_dxgi_thread.take() {
            eprintln!("[Recording] Waiting for writer thread to exit...");
            let _ = handle.join();
            eprintln!("[Recording] Writer thread stopped");
        }

        // 4. Wait for FFmpeg to finalize the file after stdin EOF
        // Since we removed audio, FFmpeg should exit quickly after video EOF
        if let Some(mut child) = self.ffmpeg_recording_handle.take() {
            eprintln!("[Recording] Waiting for FFmpeg to finalize...");

            let timeout = std::time::Duration::from_secs(10);
            let start = std::time::Instant::now();

            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("[Recording] FFmpeg exited with: {}", status);
                        break;
                    }
                    Ok(None) => {
                        if start.elapsed() > timeout {
                            eprintln!("[Recording] FFmpeg timeout, forcing kill...");
                            let _ = std::process::Command::new("taskkill")
                                .args(["/IM", "ffmpeg.exe", "/F"])
                                .output();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    Err(e) => {
                        eprintln!("[Recording] Error waiting for FFmpeg: {}", e);
                        break;
                    }
                }
            }
        }

        // 5. Reset state for next recording session
        self.recording_stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.recording_dxgi_thread = None;
        self.recording_frame_sender = None;
        self.ffmpeg_recording_handle = None;
        self.recording = false;
        self.recording_paused = false;
        let elapsed = self.recording_time;
        self.recording_time = 0;

        let duration_str = Self::format_time(elapsed);

        // Verify file was created and has proper size
        let file_exists = std::path::Path::new(&self.last_recording_path).exists();
        if file_exists {
            if let Ok(metadata) = fs::metadata(&self.last_recording_path) {
                let size_mb = metadata.len() as f64 / 1_048_576.0;
                println!(
                    "[Recording] Stopped - saved to {} ({}s, {:.2} MB)",
                    self.last_recording_path, duration_str, size_mb
                );

                // Check if file seems valid (has some size)
                if size_mb < 0.01 {
                    println!("[Recording] WARNING: File is very small, may not be playable");
                }
            } else {
                println!(
                    "[Recording] Stopped recording ({}s) saved to {}",
                    duration_str, self.last_recording_path
                );
            }
        } else {
            println!(
                "[Recording] Stopped ({}s) - WARNING: file not found at {}",
                duration_str, self.last_recording_path
            );
        }

        self.recording_start_time = None;
    }

    fn start_preview_capture(&mut self) {
        if self.preview_capture_active {
            return;
        }

        // For now, just mark as active - actual capture will use recording pipeline
        // This is a placeholder until we can integrate with the video pipeline properly
        self.preview_capture_active = true;
        eprintln!("[Preview] Preview capture requested (using recording pipeline)");
    }

    fn stop_preview_capture(&mut self) {
        if !self.preview_capture_active {
            return;
        }

        self.preview_capture_active = false;
        eprintln!("[Preview] Preview capture stopped");
    }

    /// Capture a specific monitor using our direct DXGI implementation
    /// position: (x, y) coordinates in virtual screen space - the authoritative monitor identifier
    fn capture_desktop_frame(&mut self, position: (i32, i32)) -> Option<(Vec<u8>, u32, u32)> {
        // Initialize DXGI manager if needed
        if self.dxgi_manager.is_none() {
            eprintln!("[DXGI] Initializing direct DXGI capture manager...");
            match DxgiCaptureManager::new() {
                Ok(manager) => {
                    self.dxgi_manager = Some(manager);
                    eprintln!("[DXGI] Manager initialized successfully");
                }
                Err(e) => {
                    eprintln!("[DXGI] Failed to create DXGI manager: {:?}", e);
                    return None;
                }
            }
        }

        let manager = self.dxgi_manager.as_mut().unwrap();

        // Enumerate outputs on first capture
        if !manager.has_output_at_position(position.0, position.1) {
            eprintln!("[DXGI] Enumerating all outputs...");
            match manager.enumerate_outputs() {
                Ok(outputs) => {
                    eprintln!("[DXGI] Found {} outputs total", outputs.len());
                }
                Err(e) => {
                    eprintln!("[DXGI] Enumeration failed: {:?}", e);
                    return None;
                }
            }
        }

        // Capture with DXGI - returns None on timeout (normal)
        manager
            .capture_frame(position)
            .ok()
            .map(|frame| (frame.data, frame.width, frame.height))
    }

    /// Send an already-captured frame to the recording pipeline
    /// This reuses the preview capture frame - no double capture needed
    fn send_frame_to_recording(&mut self, rgba_data: &[u8], width: u32, height: u32) {
        // Rate limit to target FPS
        let target_frame_interval = std::time::Duration::from_secs_f32(1.0 / self.fps_setting);
        static mut LAST_RECORDING_FRAME: Option<std::time::Instant> = None;

        let now = std::time::Instant::now();
        let last = unsafe { LAST_RECORDING_FRAME };
        if let Some(last_time) = last {
            if now.duration_since(last_time) < target_frame_interval {
                return; // Skip this frame - not enough time elapsed
            }
        }
        unsafe {
            LAST_RECORDING_FRAME = Some(now);
        }

        static mut FRAME_COUNT: u64 = 0;

        let out_w = self.output_width;
        let out_h = self.output_height;

        // Scale on main thread BEFORE sending to reduce memory pressure
        // 4K (33MB) → 1080p (8.3MB) = 75% reduction per frame
        let scaled_data = if width != out_w || height != out_h {
            self.scale_frame_to_output(rgba_data, width, height, out_w, out_h)
        } else {
            rgba_data.to_vec()
        };

        // Bake annotations into the frame. Annotations are stored in scene
        // output coordinates; map them onto the output-resolution frame.
        let mut scaled_data = scaled_data;
        if !self.annotations.is_empty() {
            let (scene_w, scene_h) = self
                .scenes
                .current_scene()
                .map(|s| s.output_size())
                .unwrap_or((out_w, out_h));
            if scene_w > 0 && scene_h > 0 {
                let scale_x = out_w as f32 / scene_w as f32;
                let scale_y = out_h as f32 / scene_h as f32;
                if self.record_font.is_none() {
                    self.record_font = crate::annotation_raster::load_system_font();
                }
                crate::annotation_raster::composite_annotations(
                    &mut scaled_data,
                    out_w,
                    out_h,
                    &self.annotations,
                    scale_x,
                    scale_y,
                    self.record_font.as_ref(),
                );
            }
        }

        // Convert RGBA to BGRA for FFmpeg
        let mut bgra_data = scaled_data;
        for chunk in bgra_data.chunks_exact_mut(4) {
            chunk.swap(0, 2); // RGBA -> BGRA
        }

        // Send to FFmpeg writer thread
        if let Some(ref tx) = self.recording_frame_sender {
            match tx.send(bgra_data) {
                Ok(_) => {
                    unsafe {
                        FRAME_COUNT += 1;
                    }
                    if unsafe { FRAME_COUNT } % 30 == 0 {
                        eprintln!(
                            "[DXGI-Record] Sent {} frames to FFmpeg ({}x{})",
                            unsafe { FRAME_COUNT },
                            out_w,
                            out_h
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[DXGI-Record] Channel send failed: {}, stopping recording",
                        e
                    );
                    self.stop_recording();
                }
            }
        }
    }

    fn process_preview_frames(&mut self, ctx: &egui::Context) {
        // Limit preview capture to target FPS to avoid excessive CPU usage
        // from BGRA->RGBA conversion and texture upload
        let target_frame_interval = std::time::Duration::from_secs_f32(1.0 / self.fps_setting);
        if self.last_preview_capture.elapsed() < target_frame_interval {
            return;
        }
        self.last_preview_capture = std::time::Instant::now();

        // Collect all visible capture source items first to avoid borrow issues
        let scene = self.scenes.current_scene();
        let capture_items: Vec<_> = scene
            .map(|s| {
                s.items()
                    .iter()
                    .filter(|i| {
                        i.is_visible()
                            && (i.name().starts_with("Window:")
                                || i.name().starts_with("Display Capture"))
                    })
                    .map(|i| (i.id(), i.name().to_string()))
                    .collect()
            })
            .unwrap_or_default();

        if capture_items.is_empty() {
            // No capture sources - stop preview
            if self.preview_capture_active {
                self.preview_capture_active = false;
                eprintln!("[Preview] No capture sources, stopping preview");
            }
            return;
        }

        self.preview_capture_active = true;
        self.preview_frame_count += 1;

        // Capture each source independently
        for (item_id, item_name) in &capture_items {
            let texture_key = *item_id;

            if item_name.starts_with("Window:") {
                // Window capture - use native GDI (requires HWND)
                // For now, skip window capture in preview until we store HWND in scene items
                eprintln!(
                    "[Preview] Window capture preview pending (HWND not stored in scene item)"
                );
            } else if item_name.starts_with("Display Capture") {
                // Display capture - parse monitor info from source name
                // Format: "Display Capture - Name|idx:X|x:Y|y:Z|w:W|h:H"
                // NOTE: idx is no longer used for capture - position (x,y) is the authoritative identifier
                let (mon_x, mon_y, mon_w, mon_h) = parse_monitor_coords(item_name);
                let position = (mon_x, mon_y);

                eprintln!(
                    "[Preview] Capturing Display Capture at position ({}, {}) - {}x{}",
                    mon_x, mon_y, mon_w, mon_h
                );

                // Pure DX11 capture - no GDI fallback
                if let Some((data, width, height)) = self.capture_desktop_frame(position) {
                    // Convert BGRA to RGBA (only once - DXGI returns BGRA)
                    let mut rgba_data = data;
                    for chunk in rgba_data.chunks_exact_mut(4) {
                        chunk.swap(0, 2); // BGRA -> RGBA
                    }

                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba_data,
                    );

                    // Reuse existing texture or create new one
                    if let Some(texture) = self.preview_textures.get_mut(&texture_key) {
                        texture.set(color_image, egui::TextureOptions::LINEAR);
                    } else {
                        let texture = ctx.load_texture(
                            format!("preview_{:?}", texture_key),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.preview_textures.insert(texture_key, texture);
                    }

                    // RECORDING: Reuse the captured frame for recording
                    // This eliminates double capture - recording taps into the same frame preview uses
                    if self.recording && !self.recording_paused && self.recording_frame_sender.is_some() {
                        self.send_frame_to_recording(&rgba_data, width, height);
                    }
                }
            }
        }

        // Clean up textures for sources that no longer exist
        let active_ids: std::collections::HashSet<SceneItemId> =
            capture_items.iter().map(|(id, _)| *id).collect();
        self.preview_textures
            .retain(|id, _| active_ids.contains(id));
    }

    /// Scale captured frame to output resolution (OBS-style: preview matches output)
    fn scale_frame_to_output(
        &self,
        data: &[u8],
        src_width: u32,
        src_height: u32,
        dst_width: u32,
        dst_height: u32,
    ) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};

        // If dimensions match, return original
        if src_width == dst_width && src_height == dst_height {
            return data.to_vec();
        }

        // Convert BGRA to RGBA for image crate
        let mut rgba_data = data.to_vec();
        for chunk in rgba_data.chunks_exact_mut(4) {
            chunk.swap(0, 2); // BGRA -> RGBA
        }

        // Create source image
        let src_img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            match ImageBuffer::from_raw(src_width, src_height, rgba_data) {
                Some(img) => img,
                None => {
                    eprintln!(
                        "[Scale] Failed to create {}x{} image ({} bytes)",
                        src_width,
                        src_height,
                        data.len()
                    );
                    return data.to_vec();
                }
            };

        // Resize to output resolution using bilinear filtering
        let dst_img = image::imageops::resize(
            &src_img,
            dst_width,
            dst_height,
            image::imageops::FilterType::Triangle,
        );

        // Convert back to BGRA
        let mut output = dst_img.into_raw();
        for chunk in output.chunks_exact_mut(4) {
            chunk.swap(0, 2); // RGBA -> BGRA
        }

        output
    }

    fn format_time(seconds: u64) -> String {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        let s = seconds % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New Profile").clicked() {
                        let id = self.profile_manager.write().create("New Profile".into());
                        self.profile_manager.write().set_current(id).ok();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Undo").clicked() {
                        ui.close_menu();
                    }
                    if ui.button("Redo").clicked() {
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_preview, "Preview");
                    ui.checkbox(&mut self.show_scenes, "Scenes");
                    ui.checkbox(&mut self.show_controls, "Controls");
                    ui.checkbox(&mut self.show_audio, "Audio Mixer");
                    ui.checkbox(&mut self.show_chat, "Chat");
                    ui.checkbox(&mut self.show_stats, "Stats");
                    ui.separator();
                    ui.checkbox(&mut self.show_annotations, "Annotations Toolbar");
                });
                ui.menu_button("Profile", |ui| {
                    let profiles = self.profile_manager.read().list();
                    for (id, name) in profiles {
                        if ui.button(&name).clicked() {
                            self.profile_manager.write().set_current(id).ok();
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About ROBS").clicked() {
                        ui.close_menu();
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    /// Toolbar for the annotation / mark-up tools: tool selection, stroke
    /// color, stroke width, and undo/clear. Shown as a thin panel directly
    /// below the menu bar while `show_annotations` is enabled.
    fn annotation_toolbar(&mut self, ctx: &egui::Context) {
        if !self.show_annotations {
            return;
        }

        egui::TopBottomPanel::top("annotation_toolbar")
            .exact_height(36.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // Tool buttons: Select first, then the drawable shapes.
                    let mut tools: Vec<robs_core::AnnotationTool> = vec![robs_core::AnnotationTool::Select];
                    tools.extend(
                        robs_core::AnnotationShape::ALL
                            .iter()
                            .map(|s| tool_from_shape(*s)),
                    );

                    for tool in tools {
                        let selected = self.annotation_tool == tool;
                        let label = match tool {
                            robs_core::AnnotationTool::Select => "🖱 Select".to_owned(),
                            _ => {
                                let s = tool.shape().unwrap();
                                format!("{} {}", s.icon(), s.label())
                            }
                        };
                        if ui.selectable_label(selected, label).clicked() {
                            self.annotation_tool = tool;
                            if tool == robs_core::AnnotationTool::Select {
                                self.selected_annotation = None;
                            }
                        }
                    }

                    ui.separator();

                    // Stroke color
                    let mut color = [
                        self.annotation_style.color[0],
                        self.annotation_style.color[1],
                        self.annotation_style.color[2],
                    ];
                    ui.label("Color");
                    if ui.color_edit_button_srgb(&mut color).changed() {
                        self.annotation_style.color = [color[0], color[1], color[2], 255];
                    }

                    ui.separator();

                    // Stroke width
                    ui.label("Width");
                    ui.add(
                        egui::Slider::new(&mut self.annotation_style.stroke_width, 1.0..=24.0)
                            .clamp_to_range(true),
                    );

                    // Fill toggle for closed shapes
                    let closed = self
                        .annotation_tool
                        .shape()
                        .map(|s| s.is_closed())
                        .unwrap_or(false);
                    if closed {
                        ui.checkbox(&mut self.annotation_style.filled, "Fill");
                    }

                    ui.separator();

                    // Undo / Clear
                    if ui.button("↶ Undo").clicked() {
                        self.annotations.pop();
                    }
                    if ui.button("🗑 Clear").clicked() {
                        self.annotations.clear();
                        self.selected_annotation = None;
                    }

                    ui.separator();

                    ui.label(
                        egui::RichText::new(if self.annotation_tool == robs_core::AnnotationTool::Select {
                            "Click an annotation to select, drag to move, Del to remove"
                        } else {
                            "Click and drag on the preview to draw"
                        })
                        .color(egui::Color32::from_rgb(150, 150, 150))
                        .small(),
                    );
                });
                ui.add_space(4.0);
            });
    }

    fn streaming_controls(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("streaming_controls").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // ---- Streaming controls ----
                let (stream_icon, stream_label, stream_color) = if !self.streaming {
                    ("\u{25B6}", "Start", egui::Color32::from_rgb(0, 170, 0))
                } else if self.streaming_paused {
                    ("\u{25B6}", "Resume", egui::Color32::from_rgb(0, 170, 0))
                } else {
                    ("\u{23F8}", "Pause", egui::Color32::from_rgb(210, 160, 0))
                };
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(format!("{} {}", stream_icon, stream_label))
                                .color(stream_color)
                                .strong(),
                        ),
                    )
                    .clicked()
                {
                    if !self.streaming {
                        self.streaming = true;
                        self.streaming_time = 0;
                        self.streaming_paused = false;
                    } else if self.streaming_paused {
                        self.streaming_paused = false;
                    } else {
                        self.streaming_paused = true;
                    }
                }
                if ui
                    .add_enabled(
                        self.streaming,
                        egui::Button::new(
                            egui::RichText::new("\u{23F9} Stop").color(egui::Color32::RED),
                        ),
                    )
                    .clicked()
                {
                    self.streaming = false;
                    self.streaming_paused = false;
                }

                ui.separator();

                // ---- Recording controls ----
                let (rec_icon, rec_label, rec_color) = if !self.recording {
                    ("\u{25B6}", "Start", egui::Color32::from_rgb(0, 170, 0))
                } else if self.recording_paused {
                    ("\u{25B6}", "Resume", egui::Color32::from_rgb(0, 170, 0))
                } else {
                    ("\u{23F8}", "Pause", egui::Color32::from_rgb(210, 160, 0))
                };
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(format!("{} {}", rec_icon, rec_label))
                                .color(rec_color)
                                .strong(),
                        ),
                    )
                    .clicked()
                {
                    if !self.recording {
                        self.start_recording();
                    } else if self.recording_paused {
                        self.recording_paused = false;
                    } else {
                        self.recording_paused = true;
                    }
                }
                if ui
                    .add_enabled(
                        self.recording,
                        egui::Button::new(
                            egui::RichText::new("\u{23F9} Stop").color(egui::Color32::RED),
                        ),
                    )
                    .clicked()
                {
                    self.stop_recording();
                }

                ui.separator();
                ui.label(format!("Scene: {}", self.current_scene));

                // ---- Status indicators (right-aligned) ----
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.streaming {
                        let live_text = if self.streaming_paused {
                            egui::RichText::new("\u{23F8} PAUSED").color(egui::Color32::from_rgb(210, 160, 0))
                        } else {
                            egui::RichText::new("\u{25CF} LIVE").color(egui::Color32::RED)
                        };
                        ui.label(live_text);
                        ui.label(Self::format_time(self.streaming_time));
                    }
                    if self.recording {
                        let rec_text = if self.recording_paused {
                            egui::RichText::new("\u{23F8} PAUSED").color(egui::Color32::from_rgb(210, 160, 0))
                        } else {
                            egui::RichText::new("\u{25CF} REC").color(egui::Color32::RED)
                        };
                        ui.label(rec_text);
                        ui.label(Self::format_time(self.recording_time));
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    fn show_settings_window(&mut self, ctx: &egui::Context) {
        let mut show_settings = self.show_settings;
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .resizable(true)
            .open(&mut show_settings)
            .default_width(650.0)
            .default_height(450.0)
            .min_width(400.0)
            .min_height(300.0)
            .show(ctx, |ui| {
                let available_height = ui.available_height();
                ui.columns(2, |cols| {
                    let left = &mut cols[0];
                    left.set_min_width(120.0);
                    left.set_max_width(160.0);
                    for (i, tab) in [
                        "General",
                        "Video",
                        "Audio",
                        "Hotkeys",
                        "Streaming",
                        "Outputs",
                    ]
                    .iter()
                    .enumerate()
                    {
                        if left
                            .selectable_label(self.active_settings_tab == i, *tab)
                            .clicked()
                        {
                            self.active_settings_tab = i;
                        }
                    }

                    let right = &mut cols[1];
                    egui::ScrollArea::vertical()
                        .max_height(available_height)
                        .show(right, |ui| {
                            ui.set_min_width(250.0);
                            match self.active_settings_tab {
                                0 => self.settings_general(ui),
                                1 => self.settings_video(ui),
                                2 => self.settings_audio(ui),
                                3 => self.settings_hotkeys(ui),
                                4 => self.settings_streaming(ui),
                                5 => self.settings_outputs(ui),
                                _ => {}
                            }
                        });
                });
            });
        self.show_settings = show_settings;
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        ui.heading("General");
        ui.separator();
        egui::Grid::new("settings_general").show(ui, |ui| {
            ui.label("Language:");
            ui.button("English");
            ui.end_row();
            ui.label("Theme:");
            ui.button("Dark");
            ui.end_row();
            ui.label("Confirm on exit:");
            ui.checkbox(&mut self.confirm_on_exit, "");
            ui.end_row();
            ui.label("Minimize to tray:");
            ui.checkbox(&mut self.minimize_to_tray, "");
            ui.end_row();
            ui.label("Always on top:");
            ui.checkbox(&mut self.always_on_top, "");
            ui.end_row();
            ui.label("Check for updates:");
            ui.checkbox(&mut self.check_for_updates, "");
            ui.end_row();
            ui.label("Filename formatting:");
            ui.text_edit_singleline(&mut self.filename_formatting);
            ui.end_row();
        });
    }

    fn settings_video(&mut self, ui: &mut egui::Ui) {
        ui.heading("Video");
        ui.separator();
        egui::Grid::new("settings_video").show(ui, |ui| {
            ui.label("Base Resolution (Canvas):");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.base_width)
                        .range(640..=7680)
                        .suffix(" x"),
                );
                ui.add(egui::DragValue::new(&mut self.base_height).range(480..=4320));
            });
            ui.end_row();
            ui.label("Output (Scaled) Resolution:");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.output_width)
                        .range(640..=7680)
                        .suffix(" x"),
                );
                ui.add(egui::DragValue::new(&mut self.output_height).range(480..=4320));
            });
            ui.end_row();
            ui.label("Downscale Filter:");
            ui.button("Lanczos");
            ui.end_row();
            ui.label("FPS Type:");
            ui.button("Integer");
            ui.end_row();
            ui.label("FPS:");
            ui.add(egui::Slider::new(&mut self.fps_setting, 1.0..=120.0));
            ui.end_row();
        });
        ui.separator();
        ui.label(egui::RichText::new("Note: Base Resolution is your capture canvas size. Output Resolution is what gets encoded.").small().weak());
    }

    fn settings_audio(&mut self, ui: &mut egui::Ui) {
        ui.heading("Audio");
        ui.separator();
        egui::Grid::new("settings_audio").show(ui, |ui| {
            // Sample rate selection
            ui.label("Sample Rate:");
            egui::ComboBox::from_id_salt("audio_sample_rate")
                .selected_text(&self.audio_sample_rate)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.audio_sample_rate,
                        "44100".to_string(),
                        "44.1 kHz",
                    );
                    ui.selectable_value(&mut self.audio_sample_rate, "48000".to_string(), "48 kHz");
                });
            ui.end_row();

            // Channel mode selection
            ui.label("Channels:");
            egui::ComboBox::from_id_salt("audio_channel_mode")
                .selected_text(if self.audio_channel_mode == "mono" {
                    "Mono"
                } else {
                    "Stereo"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.audio_channel_mode, "mono".to_string(), "Mono");
                    ui.selectable_value(
                        &mut self.audio_channel_mode,
                        "stereo".to_string(),
                        "Stereo",
                    );
                });
            ui.end_row();

            // Desktop Audio device selection
            ui.label("Desktop Audio:");
            let desktop_device = self
                .audio_channels
                .iter()
                .find(|ch| ch.is_desktop)
                .map(|ch| ch.device_id.clone())
                .unwrap_or_else(|| "default".to_string());

            let mut selected_desktop = desktop_device.clone();
            egui::ComboBox::from_id_salt("desktop_audio_device")
                .selected_text(&selected_desktop)
                .show_ui(ui, |ui| {
                    for device in &self.audio_devices {
                        let display_name = if device.id == "disabled" {
                            "Disabled"
                        } else if device.id == "default" {
                            "Default"
                        } else {
                            &device.name
                        };
                        ui.selectable_value(&mut selected_desktop, device.id.clone(), display_name);
                    }
                });
            // Update the device_id in audio_channels
            for ch in &mut self.audio_channels {
                if ch.is_desktop {
                    ch.device_id = selected_desktop.clone();
                }
            }
            ui.end_row();

            // Mic/Aux device selection
            ui.label("Mic/Aux:");
            let mic_device = self
                .audio_channels
                .iter()
                .find(|ch| !ch.is_desktop)
                .map(|ch| ch.device_id.clone())
                .unwrap_or_else(|| "default".to_string());

            let mut selected_mic = mic_device.clone();
            egui::ComboBox::from_id_salt("mic_aux_device")
                .selected_text(&selected_mic)
                .show_ui(ui, |ui| {
                    for device in &self.audio_devices {
                        let display_name = if device.id == "disabled" {
                            "Disabled"
                        } else if device.id == "default" {
                            "Default"
                        } else {
                            &device.name
                        };
                        ui.selectable_value(&mut selected_mic, device.id.clone(), display_name);
                    }
                });
            // Update the device_id in audio_channels
            for ch in &mut self.audio_channels {
                if !ch.is_desktop {
                    ch.device_id = selected_mic.clone();
                }
            }
            ui.end_row();
        });
    }

    fn settings_hotkeys(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hotkeys");
        ui.separator();
        ui.label("Hotkey configuration coming soon.");
    }

    fn settings_streaming(&mut self, ui: &mut egui::Ui) {
        ui.heading("Streaming");
        ui.separator();
        egui::Grid::new("settings_streaming").show(ui, |ui| {
            ui.label("Service:");
            ui.button("Twitch");
            ui.end_row();
            ui.label("Server:");
            ui.text_edit_singleline(&mut self.stream_server);
            ui.end_row();
            ui.label("Stream Key:");
            ui.text_edit_singleline(&mut self.stream_key);
            ui.end_row();
            ui.label("Video Encoder:");
            egui::ComboBox::from_id_salt("video_encoder")
                .selected_text(&self.video_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_video_encoders {
                        ui.selectable_value(&mut self.video_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Audio Encoder:");
            egui::ComboBox::from_id_salt("audio_encoder")
                .selected_text(&self.audio_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_audio_encoders {
                        ui.selectable_value(&mut self.audio_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Bitrate:");
            ui.add(egui::Slider::new(&mut self.stream_bitrate, 1000..=20000).suffix(" kbps"));
            ui.end_row();
            ui.label("Rate Control:");
            ui.button("CBR");
            ui.end_row();
            ui.label("Keyframe Interval:");
            ui.add(egui::Slider::new(&mut self.keyframe_interval, 0..=20).suffix(" s"));
            ui.end_row();
            ui.label("Preset:");
            ui.button("faster");
            ui.end_row();
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Encoder Status:").strong());
            if self.ffmpeg_available {
                ui.label(egui::RichText::new("FFmpeg OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("FFmpeg not found").color(egui::Color32::RED));
            }
            if self.nvenc_available {
                ui.label(egui::RichText::new("NVENC OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("NVENC not available").color(egui::Color32::YELLOW));
            }
            if self.aac_available {
                ui.label(egui::RichText::new("AAC OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("AAC not available").color(egui::Color32::YELLOW));
            }
        });
    }

    fn settings_outputs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Outputs");
        ui.separator();
        ui.heading("Recording");
        ui.separator();
        egui::Grid::new("settings_recording").show(ui, |ui| {
            ui.label("Type:");
            ui.button("Standard");
            ui.end_row();
            ui.label("Format:");
            egui::ComboBox::from_id_salt("recording_format")
                .selected_text(&self.recording_format)
                .show_ui(ui, |ui| {
                    for fmt in ["mp4", "mkv", "flv", "mov"] {
                        ui.selectable_value(&mut self.recording_format, fmt.to_string(), fmt);
                    }
                });
            ui.end_row();
            ui.label("Video Encoder:");
            egui::ComboBox::from_id_salt("recording_encoder")
                .selected_text(&self.video_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_video_encoders {
                        ui.selectable_value(&mut self.video_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Audio Encoder:");
            egui::ComboBox::from_id_salt("audio_encoder")
                .selected_text(&self.audio_encoder)
                .show_ui(ui, |ui| {
                    for enc in &self.available_audio_encoders {
                        ui.selectable_value(&mut self.audio_encoder, enc.clone(), enc);
                    }
                });
            ui.end_row();
            ui.label("Bitrate:");
            ui.add(egui::Slider::new(&mut self.recording_bitrate, 1000..=50000).suffix(" kbps"));
            ui.end_row();
            ui.label("Path:");
            ui.horizontal(|ui| {
                let display_path = if self.recording_path.is_empty() {
                    "(not set)".to_string()
                } else {
                    self.recording_path.clone()
                };
                ui.label(display_path);
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_directory(
                            std::env::var("USERPROFILE")
                                .unwrap_or_else(|_| "C:\\Users".to_string()),
                        )
                        .pick_folder()
                    {
                        self.recording_path = path.to_string_lossy().into_owned();
                    }
                }
            });
            ui.end_row();
        });
    }

    fn scenes_panel(&mut self, ctx: &egui::Context) {
        if self.show_scenes {
            egui::SidePanel::left("scenes_panel")
                .default_width(250.0)
                .resizable(true)
                .show_animated(ctx, true, |ui| {
                    ui.heading("Scenes");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("+ Add").clicked() {
                            let name = format!("Scene {}", self.scenes.count() + 1);
                            self.scenes.create_scene(name.clone());
                            self.scenes.set_current_scene(&name);
                            self.current_scene = name;
                        }
                        if ui.button("−").clicked() {
                            if self.scenes.count() > 1 {
                                if let Some(name) = self.scenes.current_scene_name() {
                                    let name = name.to_string(); // Copy for later use
                                    self.scenes.remove(&name);
                                    // Switch to first available scene - collect names first
                                    let scene_names: Vec<String> =
                                        self.scenes.list().iter().map(|s| s.to_string()).collect();
                                    if let Some(first) = scene_names.first() {
                                        self.scenes.set_current_scene(first);
                                        self.current_scene = first.clone();
                                    }
                                }
                            }
                        }
                    });
                    ui.separator();

                    // Collect scene list to avoid borrow issues
                    let scene_names: Vec<String> =
                        self.scenes.list().iter().map(|s| s.to_string()).collect();

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for name in &scene_names {
                            let selected = self.current_scene == *name;
                            if ui.selectable_label(selected, name).clicked() {
                                self.scenes.set_current_scene(name);
                                self.current_scene = name.clone();
                            }
                        }
                    });

                    // Show sources for current scene - get data first to avoid borrow issues
                    let scene_name = self.scenes.current_scene_name().map(|s| s.to_string());
                    if let Some(name) = scene_name {
                        ui.separator();
                        ui.heading("Sources");
                        ui.separator();
                        ui.menu_button("+ Add Source", |ui| {
                            ui.menu_button("Display Capture", |ui| {
                                let monitors = get_monitors();
                                if monitors.is_empty() {
                                    ui.label("No monitors detected");
                                } else {
                                    ui.label("Select Monitor:");
                                    for (idx, monitor) in monitors.iter().enumerate() {
                                        let label = if monitor.is_primary {
                                            format!(
                                                "{} ({}x{} - PRIMARY)",
                                                monitor.name, monitor.width, monitor.height
                                            )
                                        } else {
                                            format!(
                                                "{} ({}x{} @ {},{})",
                                                monitor.name,
                                                monitor.width,
                                                monitor.height,
                                                monitor.position_x,
                                                monitor.position_y
                                            )
                                        };
                                        if ui.button(label).clicked() {
                                            if let Some(scene) = self.scenes.current_scene_mut() {
                                                let source_id =
                                                    SourceId(robs_core::types::ObjectId::new());
                                                // Store monitor index and coordinates in the name for capture
                                                let mon_name = if monitor.is_primary {
                                                    "Primary"
                                                } else {
                                                    &monitor.name
                                                };
                                                scene.add_source(
                                                    source_id,
                                                    format!(
                                                        "Display Capture - {}|idx:{}|x:{}|y:{}|w:{}|h:{}",
                                                        mon_name,
                                                        idx,
                                                        monitor.position_x,
                                                        monitor.position_y,
                                                        monitor.width,
                                                        monitor.height
                                                    ),
                                                );
                                            }
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                            ui.menu_button("Window Capture", |ui| {
                                let windows = get_open_windows();
                                if windows.is_empty() {
                                    ui.label("No windows found");
                                } else {
                                    ui.label("Select Window:");
                                    for window in windows.iter().take(50) {
                                        let label: String = window.title.chars().take(40).collect();
                                        let label = if window.title.chars().count() > 40 {
                                            format!("{}...", label)
                                        } else {
                                            label
                                        };

                                        if ui.button(&label).clicked() {
                                            if let Some(scene) = self.scenes.current_scene_mut() {
                                                let source_id =
                                                    SourceId(robs_core::types::ObjectId::new());
                                                scene.add_source(
                                                    source_id,
                                                    format!("Window: {}", window.title),
                                                );
                                            }
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                        });

                        // Display sources in current scene - get items first
                        let items_data: Vec<_> = if let Some(scene) = self.scenes.get(&name) {
                            scene
                                .items()
                                .iter()
                                .map(|i| {
                                    let pos = i.position();
                                    let scale = i.scale();
                                    let crop = i.crop();
                                    (
                                        i.id(),
                                        i.name().to_string(),
                                        i.is_visible(),
                                        pos.x,
                                        pos.y,
                                        scale.x,
                                        scale.y,
                                        i.rotation(),
                                        crop.left,
                                        crop.top,
                                        crop.right,
                                        crop.bottom,
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };

                        for (
                            id,
                            item_name,
                            is_visible,
                            _px,
                            _py,
                            _sx,
                            _sy,
                            _rot,
                            _cl,
                            _ct,
                            _cr,
                            _cb,
                        ) in items_data
                        {
                            let mut visible = is_visible;
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut visible, "");
                                let response = ui.label(&item_name);

                                // Context menu: Remove, Properties
                                response.context_menu(|ui| {
                                    if ui.button("Properties").clicked() {
                                        // Load source properties into editing fields
                                        if let Some(scene) = self.scenes.get_mut(&name) {
                                            if let Some(item) = scene.item_mut(id) {
                                                self.editing_source_id = Some(id);
                                                self.editing_source_name = item.name().to_string();
                                                let pos = item.position();
                                                let scale = item.scale();
                                                let crop = item.crop();
                                                self.editing_source_pos_x = pos.x;
                                                self.editing_source_pos_y = pos.y;
                                                self.editing_source_scale_x = scale.x;
                                                self.editing_source_scale_y = scale.y;
                                                self.editing_source_rotation = item.rotation();
                                                self.editing_source_crop_left = crop.left;
                                                self.editing_source_crop_top = crop.top;
                                                self.editing_source_crop_right = crop.right;
                                                self.editing_source_crop_bottom = crop.bottom;
                                                self.show_source_properties = true;
                                            }
                                        }
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    // Move order controls
                                    ui.horizontal(|ui| {
                                        ui.label("Order:");
                                        if ui.small_button("↑").clicked() {
                                            if let Some(scene) = self.scenes.get_mut(&name) {
                                                scene.move_item_up(id);
                                            }
                                            ui.close_menu();
                                        }
                                        if ui.small_button("↓").clicked() {
                                            if let Some(scene) = self.scenes.get_mut(&name) {
                                                scene.move_item_down(id);
                                            }
                                            ui.close_menu();
                                        }
                                    });
                                    ui.separator();
                                    if ui.button("Remove").clicked() {
                                        if let Some(scene) = self.scenes.get_mut(&name) {
                                            scene.remove_item(id);
                                        }
                                        ui.close_menu();
                                    }
                                });
                            });
                            // Update visibility if changed
                            if visible != is_visible {
                                if let Some(scene) = self.scenes.current_scene_mut() {
                                    scene.set_item_visible(id, visible);
                                }
                            }
                        }
                    }
                });
        }
    }

    fn source_properties_modal(&mut self, ctx: &egui::Context) {
        if self.show_source_properties {
            egui::Window::new("Source Properties")
                .collapsible(false)
                .resizable(false)
                .default_width(320.0)
                .show(ctx, |ui| {
                    ui.heading(&self.editing_source_name);
                    ui.separator();

                    // Position
                    ui.label("Position");
                    ui.horizontal(|ui| {
                        ui.label("X:");
                        ui.add(egui::DragValue::new(&mut self.editing_source_pos_x).speed(1.0));
                        ui.label("Y:");
                        ui.add(egui::DragValue::new(&mut self.editing_source_pos_y).speed(1.0));
                    });

                    ui.separator();

                    // Scale
                    ui.label("Scale");
                    ui.horizontal(|ui| {
                        ui.label("X:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_scale_x)
                                .speed(0.01)
                                .clamp_range(0.01..=10.0),
                        );
                        ui.label("Y:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_scale_y)
                                .speed(0.01)
                                .clamp_range(0.01..=10.0),
                        );
                    });

                    ui.separator();

                    // Rotation
                    ui.label("Rotation");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_rotation)
                                .speed(1.0)
                                .clamp_range(0.0..=360.0),
                        );
                        ui.label("degrees");
                    });

                    ui.separator();

                    // Crop
                    ui.label("Crop (pixels)");
                    ui.horizontal(|ui| {
                        ui.label("L:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_crop_left)
                                .speed(1.0)
                                .clamp_range(0..=9999),
                        );
                        ui.label("T:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_crop_top)
                                .speed(1.0)
                                .clamp_range(0..=9999),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("R:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_crop_right)
                                .speed(1.0)
                                .clamp_range(0..=9999),
                        );
                        ui.label("B:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing_source_crop_bottom)
                                .speed(1.0)
                                .clamp_range(0..=9999),
                        );
                    });

                    ui.separator();

                    // Apply / Cancel
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            if let Some(id) = self.editing_source_id {
                                if let Some(scene) = self.scenes.current_scene_mut() {
                                    if let Some(item) = scene.item_mut(id) {
                                        item.set_position(Position::new(
                                            self.editing_source_pos_x,
                                            self.editing_source_pos_y,
                                        ));
                                        item.set_scale(Scale::new(
                                            self.editing_source_scale_x,
                                            self.editing_source_scale_y,
                                        ));
                                        item.set_rotation(self.editing_source_rotation);
                                        item.set_crop(Crop::new(
                                            self.editing_source_crop_left,
                                            self.editing_source_crop_top,
                                            self.editing_source_crop_right,
                                            self.editing_source_crop_bottom,
                                        ));
                                    }
                                }
                            }
                            self.show_source_properties = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_source_properties = false;
                        }
                    });
                });
        }
    }

    /// Render committed and in-progress annotations over the canvas and
    /// handle tool interaction (draw / select / move / delete).
    ///
    /// `canvas_rect` is the on-screen rectangle representing the whole scene
    /// and `canvas_scale` converts scene coordinates -> screen pixels (see
    /// [`Self::preview_panel`] for the canonical letterbox math).
    fn render_annotations(
        &mut self,
        ui: &mut egui::Ui,
        canvas_rect: egui::Rect,
        canvas_scale: f32,
    ) {
        if !self.show_annotations {
            return;
        }

        let to_screen = |p: Position| {
            egui::pos2(
                canvas_rect.min.x + p.x * canvas_scale,
                canvas_rect.min.y + p.y * canvas_scale,
            )
        };
        let to_scene = |sp: egui::Pos2| {
            Position::new(
                (sp.x - canvas_rect.min.x) / canvas_scale,
                (sp.y - canvas_rect.min.y) / canvas_scale,
            )
        };
        let painter = ui.painter();
        let canvas_min = canvas_rect.min;

        // Compute the screen-space bounding rect of an annotation (for
        // selection highlighting + hit-testing).
        let text_font_screen =
            (crate::annotation_raster::TEXT_FONT_SIZE * canvas_scale).max(8.0);
        let screen_bbox = |ann: &robs_core::Annotation| -> egui::Rect {
            match ann.shape() {
                robs_core::AnnotationShape::Pen => {
                    if ann.points().is_empty() {
                        return egui::Rect::NOTHING;
                    }
                    let pts: Vec<egui::Pos2> =
                        ann.points().iter().map(|&p| to_screen(p)).collect();
                    let mut r = egui::Rect::from_two_pos(pts[0], pts[0]);
                    for p in &pts[1..] {
                        r.extend_with(*p);
                    }
                    r
                }
                robs_core::AnnotationShape::Text => {
                    let anchor = to_screen(ann.start());
                    let w = (ann.text().len().max(1) as f32) * text_font_screen * 0.6;
                    egui::Rect::from_min_size(
                        anchor,
                        egui::vec2(w.max(20.0), text_font_screen * 1.3),
                    )
                }
                _ => egui::Rect::from_two_pos(to_screen(ann.start()), to_screen(ann.end())),
            }
        };

        // 1. Collect (id, index, screen-bbox) for visible annotations.
        let draw_info: Vec<(robs_core::AnnotationId, usize, egui::Rect)> = self
            .annotations
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_visible())
            .map(|(i, a)| (a.id(), i, screen_bbox(a)))
            .collect();

        let selected = self.selected_annotation;

        // 2. Paint committed annotations + selection highlight.
        for &(id, idx, bbox) in &draw_info {
            let ann = &self.annotations[idx];
            paint_annotation(painter, ann, canvas_min, canvas_scale);
            if selected == Some(id) {
                painter.rect_stroke(
                    bbox.expand(4.0),
                    2.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(0, 180, 255)),
                );
            }
        }

        // 3. Paint the in-progress annotation.
        if let Some(ann) = self.annotation_drawing.as_ref() {
            paint_annotation(painter, ann, canvas_min, canvas_scale);
        }

        // 4. Tool interaction
        let tool = self.annotation_tool;
        match tool {
            // ---- Pen: drag to accumulate freehand points ----
            robs_core::AnnotationTool::Pen => {
                let resp = ui.interact(
                    canvas_rect,
                    ui.make_persistent_id("annotation_pen_canvas"),
                    egui::Sense::drag(),
                );
                if resp.drag_started() {
                    if let Some(pos) = resp.hover_pos() {
                        let scene_pos = to_scene(pos);
                        let mut ann =
                            robs_core::Annotation::new(robs_core::AnnotationShape::Pen, scene_pos, scene_pos);
                        ann.set_style(self.annotation_style);
                        ann.push_point(scene_pos);
                        self.annotation_drawing = Some(ann);
                    }
                }
                if resp.dragged() {
                    if let Some(pos) = resp.hover_pos() {
                        if let Some(ann) = self.annotation_drawing.as_mut() {
                            ann.push_point(to_scene(pos));
                        }
                    }
                }
                if resp.drag_released() {
                    if let Some(ann) = self.annotation_drawing.take() {
                        if ann.points().len() > 1 {
                            self.annotations.push(ann);
                        }
                    }
                }
            }
            // ---- Text: click to place, then type inline ----
            robs_core::AnnotationTool::Text => {
                let resp = ui.interact(
                    canvas_rect,
                    ui.make_persistent_id("annotation_text_canvas"),
                    egui::Sense::click(),
                );
                if resp.clicked() {
                    if let Some(pos) = resp.hover_pos() {
                        let scene_pos = to_scene(pos);
                        let mut ann = robs_core::Annotation::new(
                            robs_core::AnnotationShape::Text,
                            scene_pos,
                            scene_pos,
                        );
                        ann.set_style(self.annotation_style);
                        self.annotations.push(ann);
                        let new_id = self.annotations.last().map(|a| a.id());
                        self.editing_text_id = new_id;
                        self.text_input.clear();
                    }
                }
            }
            // ---- Select: click to select, drag to move, Del to remove ----
            robs_core::AnnotationTool::Select => {
                let mut clicked_id = None;
                for &(id, _idx, bbox) in draw_info.iter().rev() {
                    let resp = ui.interact(
                        bbox.expand(6.0),
                        ui.make_persistent_id(format!("annotation_{}", id.0 .0)),
                        egui::Sense::click_and_drag(),
                    );
                    if clicked_id.is_none() && resp.clicked() {
                        clicked_id = Some(id);
                    }
                    if resp.dragged() {
                        let delta = resp.drag_delta();
                        let dx = delta.x / canvas_scale;
                        let dy = delta.y / canvas_scale;
                        if let Some(ann) =
                            self.annotations.iter_mut().find(|a| a.id() == id)
                        {
                            ann.translate(dx, dy);
                        }
                    }
                }
                if let Some(id) = clicked_id {
                    self.selected_annotation = Some(id);
                }
                if let Some(sel) = self.selected_annotation {
                    let delete_pressed = ui.input(|i| {
                        i.key_pressed(egui::Key::Delete)
                            || i.key_pressed(egui::Key::Backspace)
                    });
                    if delete_pressed {
                        self.annotations.retain(|a| a.id() != sel);
                        self.selected_annotation = None;
                    }
                }
            }
            // ---- Regular shapes (Arrow/Line/Rectangle/Ellipse): drag to create ----
            _ => {
                let shape = tool.shape().unwrap();
                let resp = ui.interact(
                    canvas_rect,
                    ui.make_persistent_id("annotation_draw_canvas"),
                    egui::Sense::drag(),
                );
                if resp.drag_started() {
                    if let Some(pos) = resp.hover_pos() {
                        let scene_pos = to_scene(pos);
                        let mut ann = robs_core::Annotation::new(shape, scene_pos, scene_pos);
                        ann.set_style(self.annotation_style);
                        self.annotation_drawing = Some(ann);
                    }
                }
                if resp.dragged() {
                    if let Some(pos) = resp.hover_pos() {
                        if let Some(ann) = self.annotation_drawing.as_mut() {
                            ann.set_end(to_scene(pos));
                        }
                    }
                }
                if resp.drag_released() {
                    if let Some(ann) = self.annotation_drawing.take() {
                        let (w, h) = ann.size();
                        if w > 2.0 || h > 2.0 {
                            self.annotations.push(ann);
                        }
                    }
                }
            }
        }

        // 5. Inline text editor for the annotation being edited.
        if let Some(edit_id) = self.editing_text_id {
            let exists = self.annotations.iter().any(|a| a.id() == edit_id);
            if !exists {
                self.editing_text_id = None;
            } else {
                let anchor = to_screen(
                    self.annotations
                        .iter()
                        .find(|a| a.id() == edit_id)
                        .map(|a| a.start())
                        .unwrap_or_default(),
                );
                let edit_rect = egui::Rect::from_min_size(
                    anchor,
                    egui::vec2(200.0, text_font_screen + 8.0),
                );
                let resp = ui.put(
                    edit_rect,
                    egui::TextEdit::singleline(&mut self.text_input)
                        .font(egui::FontId::proportional(text_font_screen))
                        .desired_width(200.0),
                );
                // Focus the field once on first appearance.
                if !resp.has_focus() && !resp.lost_focus() {
                    resp.request_focus();
                }
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if resp.lost_focus() || escape {
                    let text = std::mem::take(&mut self.text_input);
                    if text.is_empty() || escape {
                        self.annotations.retain(|a| a.id() != edit_id);
                    } else if let Some(ann) =
                        self.annotations.iter_mut().find(|a| a.id() == edit_id)
                    {
                        ann.set_text(text);
                    }
                    self.editing_text_id = None;
                }
            }
        }
    }

    fn preview_panel(&mut self, ctx: &egui::Context) {
        if self.show_preview {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();

                // Draw background
                ui.painter()
                    .rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 30, 30));

                // Get scene and calculate canvas area
                let scene = self.scenes.current_scene();
                let (scene_output_w, scene_output_h) =
                    scene.map(|s| s.output_size()).unwrap_or((1920, 1080));

                // Calculate preview canvas size (fit scene output into available rect)
                let available_size = rect.size();
                let scale_x = available_size.x / scene_output_w as f32;
                let scale_y = available_size.y / scene_output_h as f32;
                let canvas_scale = scale_x.min(scale_y) * 0.95;

                let canvas_w = scene_output_w as f32 * canvas_scale;
                let canvas_h = scene_output_h as f32 * canvas_scale;
                let canvas_off_x = (available_size.x - canvas_w) / 2.0;
                let canvas_off_y = (available_size.y - canvas_h) / 2.0;

                let canvas_rect = egui::Rect::from_min_size(
                    rect.min + egui::vec2(canvas_off_x, canvas_off_y),
                    egui::vec2(canvas_w, canvas_h),
                );

                // Draw canvas border
                ui.painter().rect_stroke(
                    canvas_rect,
                    2.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60)),
                );

                // A draw tool overrides source-item dragging so the canvas is
                // free for drawing annotations.
                let draw_active =
                    self.show_annotations && self.annotation_tool.shape().is_some();

                // Render scene items
                if let Some(scene) = scene {
                    // Collect items to avoid borrow issues
                    let items: Vec<_> = scene
                        .items()
                        .iter()
                        .map(|i| {
                            let pos = i.position();
                            let scale = i.scale();
                            let crop = i.crop();
                            (
                                i.id(),
                                i.name().to_string(),
                                i.is_visible(),
                                pos.x,
                                pos.y,
                                scale.x,
                                scale.y,
                                crop.left,
                                crop.top,
                                crop.right,
                                crop.bottom,
                            )
                        })
                        .collect();

                    for (id, name, visible, px, py, sx, sy, _cl, _ct, _cr, _cb) in items {
                        if !visible {
                            continue;
                        }

                        // Determine source dimensions
                        let (src_w, src_h) = if name.starts_with("Display Capture") {
                            // Parse actual monitor dimensions from source name
                            let (_, _, w, h) = parse_monitor_coords(&name);
                            (w as f32, h as f32)
                        } else {
                            // Window capture - use scene output resolution as default
                            (scene_output_w as f32, scene_output_h as f32)
                        };

                        if src_w == 0.0 || src_h == 0.0 {
                            continue;
                        }

                        // Calculate rendered size after scaling
                        let render_w = src_w * sx;
                        let render_h = src_h * sy;

                        // Calculate position on canvas (scene coords -> canvas coords)
                        let item_x = canvas_rect.min.x + px * canvas_scale;
                        let item_y = canvas_rect.min.y + py * canvas_scale;
                        let item_w = render_w * canvas_scale;
                        let item_h = render_h * canvas_scale;

                        let item_rect = egui::Rect::from_min_size(
                            egui::pos2(item_x, item_y),
                            egui::vec2(item_w, item_h),
                        );

                        // Draw source content - look up texture by SceneItemId
                        if let Some(texture) = self.preview_textures.get(&id) {
                            ui.painter().image(
                                texture.id(),
                                item_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        } else {
                            // Placeholder: draw colored rect with source name
                            let color = if name.starts_with("Display Capture") {
                                egui::Color32::from_rgb(40, 60, 80)
                            } else {
                                egui::Color32::from_rgb(60, 40, 80)
                            };
                            ui.painter().rect_filled(item_rect, 2.0, color);
                            ui.painter().text(
                                item_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                &name,
                                egui::FontId::proportional(12.0),
                                egui::Color32::LIGHT_GRAY,
                            );
                        }

                        // Draw selection border
                        ui.painter().rect_stroke(
                            item_rect,
                            2.0,
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(0, 120, 255)),
                        );

                        // Make item draggable (disabled while drawing annotations)
                        if !draw_active {
                            let response = ui.interact(
                                item_rect,
                                ui.make_persistent_id(format!("source_item_{}", id.0 .0)),
                                egui::Sense::drag(),
                            );

                            if response.dragged() {
                                let drag_delta = response.drag_delta();
                                let delta_scene_x = drag_delta.x / canvas_scale;
                                let delta_scene_y = drag_delta.y / canvas_scale;

                                if let Some(scene) = self.scenes.current_scene_mut() {
                                    if let Some(item) = scene.item_mut(id) {
                                        let pos = item.position();
                                        item.set_position(Position::new(
                                            pos.x + delta_scene_x,
                                            pos.y + delta_scene_y,
                                        ));
                                    }
                                }
                            }

                            // Draw resize handles at corners
                            let handle_size = 8.0;
                            let corners = [
                                (item_rect.min, "tl"),
                                (egui::pos2(item_rect.max.x, item_rect.min.y), "tr"),
                                (egui::pos2(item_rect.min.x, item_rect.max.y), "bl"),
                                (item_rect.max, "br"),
                            ];

                            for (corner, corner_name) in corners {
                                let handle_rect = egui::Rect::from_center_size(
                                    corner,
                                    egui::vec2(handle_size, handle_size),
                                );
                                ui.painter()
                                    .rect_filled(handle_rect, 1.0, egui::Color32::WHITE);
                                ui.painter().rect_stroke(
                                    handle_rect,
                                    1.0,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 120, 255)),
                                );

                                // Make corner handle draggable for resize
                                let drag_response = ui.interact(
                                    handle_rect,
                                    ui.make_persistent_id(format!(
                                        "resize_{}_{}",
                                        id.0 .0, corner_name
                                    )),
                                    egui::Sense::drag(),
                                );

                                if drag_response.dragged() {
                                    let drag_delta = drag_response.drag_delta();
                                    let delta_w = drag_delta.x / canvas_scale;
                                    let delta_h = drag_delta.y / canvas_scale;

                                    if let Some(scene) = self.scenes.current_scene_mut() {
                                        if let Some(item) = scene.item_mut(id) {
                                            let scale = item.scale();
                                            let new_sx = (scale.x + delta_w / src_w).max(0.01);
                                            let new_sy = (scale.y + delta_h / src_h).max(0.01);
                                            item.set_scale(Scale::new(new_sx, new_sy));
                                        }
                                    }
                                }
                            }
                        }

                        // Draw source label
                        ui.painter().text(
                            egui::pos2(item_rect.min.x, item_rect.min.y - 16.0),
                            egui::Align2::LEFT_BOTTOM,
                            &name,
                            egui::FontId::proportional(11.0),
                            egui::Color32::from_rgb(180, 180, 180),
                        );
                    }
                } else {
                    let center = rect.center();
                    ui.painter().text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "No Active Scene",
                        egui::FontId::proportional(24.0),
                        egui::Color32::GRAY,
                    );
                }

                // Annotation / mark-up overlay and tool interaction
                self.render_annotations(ui, canvas_rect, canvas_scale);

                // Show recording indicator
                if self.recording {
                    let live_rect = egui::Rect::from_min_size(
                        rect.min + egui::vec2(10.0, 10.0),
                        egui::vec2(60.0, 25.0),
                    );
                    ui.painter().rect_filled(live_rect, 4.0, egui::Color32::RED);
                    ui.painter().text(
                        live_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "REC",
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                    );
                }
            });
        }
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        if self.show_audio || self.show_chat || self.show_stats || self.show_controls {
            egui::SidePanel::right("right_panel")
                .default_width(300.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if self.show_controls {
                            ui.collapsing("Controls", |ui| {
                                ui.vertical_centered(|ui| {
                                    // ---- Streaming controls ----
                                    let (s_icon, s_label, s_color) = if !self.streaming {
                                        ("\u{25B6}", "Start", egui::Color32::from_rgb(0, 170, 0))
                                    } else if self.streaming_paused {
                                        ("\u{25B6}", "Resume", egui::Color32::from_rgb(0, 170, 0))
                                    } else {
                                        ("\u{23F8}", "Pause", egui::Color32::from_rgb(210, 160, 0))
                                    };
                                    if ui.add(egui::Button::new(
                                        egui::RichText::new(format!("{} Stream", s_icon)).color(s_color).strong(),
                                    )).clicked() {
                                        if !self.streaming {
                                            self.streaming = true;
                                            self.streaming_time = 0;
                                            self.streaming_paused = false;
                                        } else if self.streaming_paused {
                                            self.streaming_paused = false;
                                        } else {
                                            self.streaming_paused = true;
                                        }
                                    }
                                    if ui.add_enabled(
                                        self.streaming,
                                        egui::Button::new(egui::RichText::new("\u{23F9} Stop Stream").color(egui::Color32::RED)),
                                    ).clicked() {
                                        self.streaming = false;
                                        self.streaming_paused = false;
                                    }

                                    ui.separator();

                                    // ---- Recording controls ----
                                    let (r_icon, r_label, r_color) = if !self.recording {
                                        ("\u{25B6}", "Start", egui::Color32::from_rgb(0, 170, 0))
                                    } else if self.recording_paused {
                                        ("\u{25B6}", "Resume", egui::Color32::from_rgb(0, 170, 0))
                                    } else {
                                        ("\u{23F8}", "Pause", egui::Color32::from_rgb(210, 160, 0))
                                    };
                                    if ui.add(egui::Button::new(
                                        egui::RichText::new(format!("{} Record", r_icon)).color(r_color).strong(),
                                    )).clicked() {
                                        if !self.recording {
                                            self.start_recording();
                                        } else if self.recording_paused {
                                            self.recording_paused = false;
                                        } else {
                                            self.recording_paused = true;
                                        }
                                    }
                                    if ui.add_enabled(
                                        self.recording,
                                        egui::Button::new(egui::RichText::new("\u{23F9} Stop Record").color(egui::Color32::RED)),
                                    ).clicked() {
                                        self.stop_recording();
                                    }
                                    ui.separator();
                                    if ui.button("Studio Mode").clicked() {}
                                    if ui.button("Settings").clicked() {
                                        self.show_settings = true;
                                    }
                                });
                            });
                        }

                        if self.show_audio {
                            ui.collapsing("Audio Mixer", |ui| {
                                for ch in self.audio_channels.iter_mut() {
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut ch.muted, "M");
                                        ui.label(&ch.name);
                                        ui.add(
                                            egui::Slider::new(&mut ch.volume, 0.0..=1.0)
                                                .show_value(false),
                                        );
                                        ui.label(format!("{:.0}%", ch.volume * 100.0));
                                    });
                                    let vol = ch.volume;
                                    let meter_color = if vol > 0.9 {
                                        egui::Color32::RED
                                    } else if vol > 0.7 {
                                        egui::Color32::YELLOW
                                    } else {
                                        egui::Color32::GREEN
                                    };
                                    let meter_rect = ui.available_rect_before_wrap();
                                    let meter = egui::Rect::from_min_size(
                                        meter_rect.min,
                                        egui::vec2(meter_rect.width() * vol, 6.0),
                                    );
                                    ui.painter().rect_filled(
                                        meter,
                                        1.0,
                                        meter_color.gamma_multiply(0.7),
                                    );
                                    ui.add_space(10.0);
                                }
                            });
                        }

                        if self.show_chat {
                            ui.collapsing("Chat", |ui| {
                                egui::ScrollArea::vertical()
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        let messages = self.chat_messages.read();
                                        if messages.is_empty() {
                                            ui.label(
                                                egui::RichText::new("No messages")
                                                    .color(egui::Color32::GRAY),
                                            );
                                        } else {
                                            for msg in messages.iter() {
                                                ui.horizontal(|ui| {
                                                    let color = msg
                                                        .user
                                                        .color
                                                        .as_ref()
                                                        .and_then(|c| {
                                                            egui::Color32::from_hex(c).ok()
                                                        })
                                                        .unwrap_or(egui::Color32::WHITE);
                                                    ui.label(
                                                        egui::RichText::new(&msg.user.display_name)
                                                            .color(color)
                                                            .strong(),
                                                    );
                                                    ui.label(":");
                                                    ui.label(&msg.content);
                                                });
                                            }
                                        }
                                    });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.text_edit_singleline(&mut self.chat_input);
                                    if ui.button("Send").clicked() && !self.chat_input.is_empty() {
                                        self.chat_input.clear();
                                    }
                                });
                            });
                        }

                        if self.show_stats {
                            ui.collapsing("Stats", |ui| {
                                egui::Grid::new("stats").show(ui, |ui| {
                                    ui.label("Streaming:");
                                    ui.label(if self.streaming { "Active" } else { "Inactive" });
                                    ui.end_row();
                                    ui.label("Recording:");
                                    ui.label(if self.recording { "Active" } else { "Inactive" });
                                    ui.end_row();
                                    if self.streaming {
                                        ui.label("Duration:");
                                        ui.label(Self::format_time(self.streaming_time));
                                        ui.end_row();
                                    }
                                    ui.label("Frame Rate:");
                                    ui.label(format!("{:.1} fps", self.fps));
                                    ui.end_row();
                                    ui.label("Bitrate:");
                                    ui.label(format!("{} kbps", self.bitrate));
                                    ui.end_row();
                                    ui.label("Dropped Frames:");
                                    ui.label(format!("{}", self.dropped_frames));
                                    ui.end_row();
                                });
                            });
                        }
                    });
                });
        }
    }
}

impl eframe::App for RobsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events(ctx);

        // Process preview frames (recording now hooks into this - no separate capture needed)
        self.process_preview_frames(ctx);

        // Manage preview capture based on source visibility
        let has_monitor_source = self
            .scenes
            .current_scene()
            .map(|s| {
                s.items().iter().any(|i| {
                    i.is_visible()
                        && (i.name().starts_with("Window:")
                            || i.name().starts_with("Display Capture"))
                })
            })
            .unwrap_or(false);

        if has_monitor_source && !self.preview_capture_active {
            self.start_preview_capture();
        } else if !has_monitor_source && self.preview_capture_active {
            self.stop_preview_capture();
        }

        self.menu_bar(ctx);
        self.annotation_toolbar(ctx);
        self.streaming_controls(ctx);
        self.scenes_panel(ctx);
        self.source_properties_modal(ctx);
        self.preview_panel(ctx);
        self.right_panel(ctx);

        if self.show_settings {
            self.show_settings_window(ctx);
        }
    }
}

/// Map an [`robs_core::AnnotationShape`] to its corresponding drawing tool.
fn tool_from_shape(shape: robs_core::AnnotationShape) -> robs_core::AnnotationTool {
    match shape {
        robs_core::AnnotationShape::Arrow => robs_core::AnnotationTool::Arrow,
        robs_core::AnnotationShape::Line => robs_core::AnnotationTool::Line,
        robs_core::AnnotationShape::Rectangle => robs_core::AnnotationTool::Rectangle,
        robs_core::AnnotationShape::Ellipse => robs_core::AnnotationTool::Ellipse,
        robs_core::AnnotationShape::Pen => robs_core::AnnotationTool::Pen,
        robs_core::AnnotationShape::Text => robs_core::AnnotationTool::Text,
    }
}

/// Paint a single annotation shape in screen coordinates.
///
/// `canvas_min` + `canvas_scale` are used to transform the annotation's
/// scene-space control points / polyline / text anchor into screen pixels.
fn paint_annotation(
    painter: &egui::Painter,
    ann: &robs_core::Annotation,
    canvas_min: egui::Pos2,
    canvas_scale: f32,
) {
    let to_screen = |p: Position| {
        egui::pos2(
            canvas_min.x + p.x * canvas_scale,
            canvas_min.y + p.y * canvas_scale,
        )
    };

    let style = ann.style();
    let color = egui::Color32::from_rgba_unmultiplied(
        style.color[0],
        style.color[1],
        style.color[2],
        style.color[3],
    );
    let stroke = egui::Stroke::new(style.stroke_width, color);
    let start = to_screen(ann.start());
    let end = to_screen(ann.end());

    match ann.shape() {
        robs_core::AnnotationShape::Line => {
            painter.line_segment([start, end], stroke);
        }
        robs_core::AnnotationShape::Arrow => {
            painter.line_segment([start, end], stroke);
            let delta = end - start;
            let dist = delta.length();
            if dist > 1.0 {
                let dir = delta / dist;
                let head_len = (stroke.width * 3.0).max(12.0);
                let head_ang = 0.5_f32;
                let perp = egui::vec2(-dir.y, dir.x);
                let back = end - dir * head_len;
                let left = back + perp * (head_len * head_ang.tan());
                let right = back - perp * (head_len * head_ang.tan());
                painter.line_segment([end, left], stroke);
                painter.line_segment([end, right], stroke);
            }
        }
        robs_core::AnnotationShape::Rectangle => {
            let r = egui::Rect::from_two_pos(start, end);
            if style.filled {
                painter.rect_filled(r, 0.0, color);
            }
            painter.rect_stroke(r, 0.0, stroke);
        }
        robs_core::AnnotationShape::Ellipse => {
            let r = egui::Rect::from_two_pos(start, end);
            let center = r.center();
            let rx = (r.width() / 2.0).abs();
            let ry = (r.height() / 2.0).abs();
            let n = 48usize;
            let points: Vec<egui::Pos2> = (0..n)
                .map(|i| {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    center + egui::vec2(rx * t.cos(), ry * t.sin())
                })
                .collect();
            if style.filled {
                painter.add(egui::epaint::Shape::convex_polygon(
                    points.clone(),
                    color,
                    stroke,
                ));
            }
            for w in points.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
            if n >= 2 {
                painter.line_segment([points[n - 1], points[0]], stroke);
            }
        }
        robs_core::AnnotationShape::Pen => {
            let pts: Vec<egui::Pos2> = ann.points().iter().map(|&p| to_screen(p)).collect();
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
        }
        robs_core::AnnotationShape::Text => {
            let text = ann.text();
            if !text.is_empty() {
                let font_size =
                    (crate::annotation_raster::TEXT_FONT_SIZE * canvas_scale).max(8.0);
                let font = egui::FontId::proportional(font_size);
                painter.text(start, egui::Align2::LEFT_TOP, text, font, color);
            }
        }
    }
}
