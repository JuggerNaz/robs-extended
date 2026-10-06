//! RTMP streaming start/stop. Mirrors the FFmpeg pipeline in `record.rs`,
//! replacing the file muxer with an FLV push to the configured RTMP ingest
//! (YouTube by default, see Settings → Streaming).
//!
//! Frames arrive the same way they do for recording: the UI tick composes one
//! output-resolution BGRA frame (`capture.rs`) and sends it over an mpsc
//! channel that a dedicated writer thread drains into FFmpeg's stdin.

use super::state::{EventLogKind, StreamState};
use super::RobsController;
use std::process::Stdio;

/// Join an RTMP server URL and stream key into the full push URL.
///
/// Pure string helper so it can be unit-tested without touching the network.
/// Trims whitespace from both parts and collapses any trailing `/` on the
/// server so `rtmp://host/app/` + `key` does not become `.../app//key`.
fn build_rtmp_url(server: &str, key: &str) -> Result<String, String> {
    let server = server.trim();
    let key = key.trim();
    if server.is_empty() {
        return Err("stream server is not set".into());
    }
    if key.is_empty() {
        return Err("stream key is not set".into());
    }
    Ok(format!("{}/{}", server.trim_end_matches('/'), key))
}

/// Facade-provided configuration for starting a stream. The streaming
/// settings stay flat fields on `RobsController` (view-facing); they are
/// passed in explicitly because the service owns only the pipeline state.
pub(crate) struct StreamStartParams {
    pub server: String,
    pub key: String,
    /// The selected encoder label (e.g. `"NVIDIA NVENC H.264 (Hardware)"`).
    pub encoder_setting: String,
    pub output_width: u32,
    pub output_height: u32,
    pub fps: f32,
    pub bitrate_kbps: u32,
    pub keyframe_interval: u32,
}

/// Service owning the streaming pipeline state cluster ([`StreamState`]):
/// the FFmpeg child, its writer thread, the stop flag, and the frame
/// channel. The `streaming` / `streaming_paused` / `streaming_time` view
/// fields stay on the facade — the facade delegate keeps them in sync
/// around `start`/`stop`.
pub struct StreamService {
    state: StreamState,
}

impl StreamService {
    pub(crate) fn new() -> Self {
        Self {
            state: StreamState {
                ffmpeg_handle: None,
                writer_thread: None,
                stop_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                frame_sender: None,
                timer_last_tick: None,
                frame_count: 0,
            },
        }
    }

    /// Spawn the FFmpeg RTMP pipeline plus its frame-writer thread.
    /// Returns `Err` carrying the complete event-log line on failure —
    /// logging stays with the facade because the event log is facade state.
    pub(crate) fn start(&mut self, params: &StreamStartParams) -> Result<(), String> {
        let url = match build_rtmp_url(&params.server, &params.key) {
            Ok(url) => url,
            Err(reason) => {
                return Err(format!(
                    "Stream not started: {reason} (Settings \u{2192} Streaming)"
                ));
            }
        };

        let encoder = if params
            .encoder_setting
            .contains("NVENC")
            || params.encoder_setting.contains("NVIDIA")
        {
            "h264_nvenc"
        } else {
            "libx264"
        };

        let output_w = params.output_width;
        let output_h = params.output_height;

        let mut args: Vec<String> = Vec::new();

        // Input: raw BGRA frames at output resolution, same as the recording
        // pipeline (frames are already scaled when they reach us).
        args.extend(["-f", "rawvideo", "-pix_fmt", "bgra"].map(String::from));
        args.push("-video_size".into());
        args.push(format!("{}x{}", output_w, output_h));
        args.push("-framerate".into());
        args.push(params.fps.to_string());
        args.push("-i".into());
        args.push("pipe:0".into());

        // Video codec + FLV-compatible pixel format (YouTube/Twitch ingest
        // expects yuv420p; BGRA input would be rejected or misrendered).
        args.push("-c:v".into());
        args.push(encoder.to_string());
        args.push("-pix_fmt".into());
        args.push("yuv420p".into());

        // Streaming-tuned rate control: CBR is what live ingest wants.
        let bitrate = params.bitrate_kbps;
        let gop = ((params.fps * params.keyframe_interval as f32) as u32).max(1);
        if encoder == "h264_nvenc" {
            args.extend(["-preset", "p4", "-tune", "ll", "-gpu", "0", "-rc", "cbr"].map(String::from));
        } else {
            args.extend(["-preset", "veryfast", "-tune", "zerolatency"].map(String::from));
        }
        args.push("-b:v".into());
        args.push(format!("{}k", bitrate));
        args.push("-maxrate".into());
        args.push(format!("{}k", bitrate));
        args.push("-bufsize".into());
        args.push(format!("{}k", bitrate * 2));
        args.push("-g".into());
        args.push(gop.to_string());
        args.push("-r".into());
        args.push(params.fps.to_string());

        // Output: FLV over RTMP.
        args.push("-f".into());
        args.push("flv".into());
        args.push(url.clone());

        eprintln!("[Stream] FFmpeg args: {:?}", args);

        let spawn = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawn {
            Ok(child) => child,
            Err(e) => {
                return Err(format!("Stream failed to start: {e}"));
            }
        };

        let pid = child.id();
        eprintln!("[Stream] FFmpeg started (PID: {pid}) pushing to {url}");

        // Drain stderr on a background thread: RTMP connect/auth failures are
        // reported there, and an undrained pipe would back-pressure FFmpeg on
        // a long stream. Mirror to stderr so failures are visible in console.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("[Stream:ffmpeg] {line}");
                }
            });
        }

        // Writer thread: drain the frame channel into FFmpeg stdin (identical
        // to the recording writer; stdin EOF on exit lets FFmpeg finalize).
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let stop_flag = self.stop_flag.clone();
        let stdin = child
            .stdin
            .take()
            .expect("FFmpeg stdin should be piped");
        let writer = std::thread::spawn(move || {
            let mut stdin = stdin;
            let mut frame_count: u64 = 0;
            while !stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(frame) => {
                        if std::io::Write::write_all(&mut stdin, &frame).is_err() {
                            eprintln!("[Stream] Failed to write frame to FFmpeg");
                            break;
                        }
                        frame_count += 1;
                        if frame_count.is_multiple_of(300) {
                            eprintln!("[Stream] Written {frame_count} frames to FFmpeg");
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            eprintln!("[Stream] Writer thread exiting after {frame_count} frames");
        });

        self.ffmpeg_handle = Some(child);
        self.writer_thread = Some(writer);
        self.frame_sender = Some(tx);
        self.frame_count = 0;

        Ok(())
    }

    /// Tear down the pipeline: 1. signal the writer thread, 2. close the
    /// channel, 3. join the writer (dropping FFmpeg stdin = EOF), 4. reap
    /// the child, then reset for the next session. The flat view fields
    /// (`streaming` etc.) stay with the facade delegate.
    pub(crate) fn stop(&mut self) {
        eprintln!("[Stream] Stopping stream...");

        self.stop_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.frame_sender = None;
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }

        if let Some(mut child) = self.ffmpeg_handle.take() {
            let timeout = std::time::Duration::from_secs(10);
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("[Stream] FFmpeg exited with: {status}");
                        break;
                    }
                    Ok(None) => {
                        if start.elapsed() > timeout {
                            eprintln!("[Stream] FFmpeg timeout, killing this process only");
                            // Never `taskkill /IM ffmpeg.exe` here — it would
                            // also kill a concurrent recording's FFmpeg.
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    Err(e) => {
                        eprintln!("[Stream] Error waiting for FFmpeg: {e}");
                        break;
                    }
                }
            }
        }

        // Reset for the next session.
        self.stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.writer_thread = None;
        self.frame_sender = None;
        self.ffmpeg_handle = None;
    }
}

impl std::ops::Deref for StreamService {
    type Target = StreamState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl std::ops::DerefMut for StreamService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl RobsController {
    pub fn start_streaming(&mut self) {
        if self.streaming {
            return;
        }

        let params = StreamStartParams {
            server: self.stream_server.clone(),
            key: self.stream_key.clone(),
            encoder_setting: self.video_encoder.clone(),
            output_width: self.output_width,
            output_height: self.output_height,
            fps: self.fps_setting,
            bitrate_kbps: self.stream_bitrate,
            keyframe_interval: self.keyframe_interval,
        };
        match self.stream.start(&params) {
            Ok(()) => {
                self.streaming = true;
                self.streaming_paused = false;
                self.streaming_time = 0;
                self.bitrate = self.stream_bitrate;

                let host = self.stream_server.trim_start_matches("rtmp://").to_string();
                self.log_event(
                    format!("Streaming started \u{2192} {} ({})", host, self.stream_service),
                    EventLogKind::Stream,
                );
            }
            Err(message) => self.log_event(message, EventLogKind::Stream),
        }
    }

    pub fn stop_streaming(&mut self) {
        eprintln!("[Stream] Stopping stream...");

        let was_streaming = self.streaming;
        let elapsed = self.streaming_time / 1000; // ms → s
        self.stream.stop();
        self.streaming = false;
        self.streaming_paused = false;
        self.streaming_time = 0;

        if was_streaming {
            let duration = Self::format_time(elapsed);
            self.log_event(
                format!("Streaming stopped ({duration})"),
                EventLogKind::Stream,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build_rtmp_url;

    #[test]
    fn joins_server_and_key() {
        assert_eq!(
            build_rtmp_url("rtmp://a.rtmp.youtube.com/live2", "abc123-xyz").unwrap(),
            "rtmp://a.rtmp.youtube.com/live2/abc123-xyz"
        );
    }

    #[test]
    fn trims_whitespace_and_trailing_slash() {
        assert_eq!(
            build_rtmp_url("  rtmp://live.twitch.tv/app/ ", " k3y \n").unwrap(),
            "rtmp://live.twitch.tv/app/k3y"
        );
    }

    #[test]
    fn empty_server_is_an_error() {
        assert!(build_rtmp_url("", "key").is_err());
        assert!(build_rtmp_url("   ", "key").is_err());
    }

    #[test]
    fn empty_key_is_an_error() {
        assert!(build_rtmp_url("rtmp://host/app", "").is_err());
        assert!(build_rtmp_url("rtmp://host/app", "  ").is_err());
    }

    #[test]
    fn does_not_touch_key_slashes() {
        // A key containing a slash is the user's problem; we only normalize
        // the server side and must not mangle the key itself.
        assert_eq!(
            build_rtmp_url("rtmp://host/app", "a/b").unwrap(),
            "rtmp://host/app/a/b"
        );
    }
}
