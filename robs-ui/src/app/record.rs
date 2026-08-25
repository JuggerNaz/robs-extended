//! FFmpeg-based recording start/stop. Extracted verbatim from `app.rs`.

use robs_core::scene::CaptureSource;
use super::state::EventLogKind;
use super::RobsApp;
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;

impl RobsApp {
    pub(crate) fn start_recording(&mut self) {
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

        self.record.last_recording_path = full_path.to_string_lossy().into_owned();

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
                // Find the first visible capture source's typed metadata.
                let capture = scene
                    .items()
                    .iter()
                    .filter(|i| i.is_visible())
                    .find_map(|i| i.capture().cloned());

                match capture {
                    Some(CaptureSource::Window { title }) => {
                        eprintln!("[Recording] Found window_capture source: {}", title);
                        (format!("title={}", title), 0, 0, 0, 0, true)
                    }
                    Some(CaptureSource::Display { x, y, width, height, label }) => {
                        eprintln!("[Recording] Found monitor_capture source: {}", label);
                        eprintln!(
                            "[Recording] Using monitor at ({},{}) - {}x{}",
                            x, y, width, height
                        );
                        ("desktop".to_string(), x, y, width as i32, height as i32, false)
                    }
                    Some(CaptureSource::Webcam { device, width, height }) => {
                        eprintln!(
                            "[Recording] Found webcam source: {} ({}x{})",
                            device, width, height
                        );
                        ("webcam".to_string(), 0, 0, width as i32, height as i32, false)
                    }
                    None => {
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

        let output_path = self.record.last_recording_path.clone();

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
                    let _stop_flag = self.record.recording_stop_flag.clone();
                    let _position = (offset_x, offset_y);
                    let _input_width = video_width as u32;
                    let _input_height = video_height as u32;
                    let _fps = self.fps_setting;

                    // For DXGI recording: spawn a writer thread that reads frames from a channel
                    // and pipes them to FFmpeg stdin. The main thread captures frames via existing DXGI.
                    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
                    self.record.recording_frame_sender = Some(tx);

                    let stop_flag = self.record.recording_stop_flag.clone();
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

                    self.record.recording_dxgi_thread = Some(thread_handle);
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
                self.record.ffmpeg_recording_handle = Some(child);
                eprintln!(
                    "[Recording] Handle stored: {:?}",
                    self.record.ffmpeg_recording_handle.is_some()
                );
            }
            Err(e) => {
                println!("[Recording] Failed to start FFmpeg: {}", e);
            }
        }

        self.record.recording = true;
        self.record.recording_paused = false;
        self.record.recording_time = 0;
        self.record.frame_count = 0;
        self.record.last_frame_time = None;
        // Never duplicate a stale frame from a previous session into this one.
        self.preview.last_output_frame = None;
        self.record.recording_start_time = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        self.log_event(
            format!("Recording started: {}", self.record.last_recording_path),
            EventLogKind::Record,
        );
    }

    pub(crate) fn stop_recording(&mut self) {
        eprintln!("[Recording] Stopping recording...");

        // 1. Signal the writer thread to stop
        self.record.recording_stop_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // 2. Drop the sender to close the channel - this signals the writer thread
        self.record.recording_frame_sender = None;

        // 3. Wait for the writer thread to finish (it will drop FFmpeg stdin = EOF)
        if let Some(handle) = self.record.recording_dxgi_thread.take() {
            eprintln!("[Recording] Waiting for writer thread to exit...");
            let _ = handle.join();
            eprintln!("[Recording] Writer thread stopped");
        }

        // 4. Wait for FFmpeg to finalize the file after stdin EOF
        // Since we removed audio, FFmpeg should exit quickly after video EOF
        if let Some(mut child) = self.record.ffmpeg_recording_handle.take() {
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
        self.record.recording_stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.record.recording_dxgi_thread = None;
        self.record.recording_frame_sender = None;
        self.record.ffmpeg_recording_handle = None;
        self.record.recording = false;
        self.record.recording_paused = false;
        let elapsed = self.record.recording_time / 1000; // ms → s
        self.record.recording_time = 0;

        let duration_str = Self::format_time(elapsed);

        // Verify file was created and has proper size
        let file_exists = std::path::Path::new(&self.record.last_recording_path).exists();
        if file_exists {
            if let Ok(metadata) = fs::metadata(&self.record.last_recording_path) {
                let size_mb = metadata.len() as f64 / 1_048_576.0;
                println!(
                    "[Recording] Stopped - saved to {} ({}s, {:.2} MB)",
                    self.record.last_recording_path, duration_str, size_mb
                );

                // Check if file seems valid (has some size)
                if size_mb < 0.01 {
                    println!("[Recording] WARNING: File is very small, may not be playable");
                }
            } else {
                println!(
                    "[Recording] Stopped recording ({}s) saved to {}",
                    duration_str, self.record.last_recording_path
                );
            }
        } else {
            println!(
                "[Recording] Stopped ({}s) - WARNING: file not found at {}",
                duration_str, self.record.last_recording_path
            );
        }

        self.record.recording_start_time = None;
        self.log_event(
            format!("Recording stopped ({})", duration_str),
            EventLogKind::Record,
        );
    }
}
